//! [`Model`]: the state of a running island, independent of any GUI toolkit.
//!
//! A backend (such as `kettle-ui-iced`) renders [`Model::root`], feeds user
//! input back through the `set_*` methods, runs the [`Job`]s they return off
//! the UI thread, and delivers [`StateUpdate`]s from [`Updates`]. When the
//! user presses a submit button, it calls [`Model::submit`], then
//! [`Model::take_submitted`] after that and after every [`Model::finish`].
//! When the user presses an action button, it calls [`Model::run_action`].

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::panic::{self, AssertUnwindSafe};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures_channel::mpsc;
use futures_core::Stream;

use crate::widget::Widget;
use crate::{
    ActionFn, ActionSpec, Contents, DeriveFn, DeriveSpec, Field, Fields, FrozenIsland, Value,
    misuse, strict,
};

/// What [`IslandBuilder::build`](crate::IslandBuilder::build) produces.
pub struct Built {
    /// The island's state.
    pub model: Model,
    /// Computes the first derived values. Run them like any other [`Job`].
    pub jobs: Vec<Job>,
    /// Updates sent through [`StateHandle`](crate::StateHandle)s. Pass each
    /// one to [`Model::apply`].
    pub updates: Updates,
}

/// The state of a running island: its widget tree, field values and named
/// state.
pub struct Model {
    root: Arc<Widget>,
    fields: Fields,
    /// All named state, derived or not.
    state: HashMap<String, Slot>,
    /// In dependency order: each comes after the derived state it depends on.
    derived: Vec<Derived>,
    actions: Vec<ActionSlot>,
    /// The named state with an initial value, which actions may set.
    settable: Arc<HashSet<String>>,
    /// Whether the user has tried to submit, so validation errors are shown.
    errors_shown: bool,
    /// Whether a submit waits for validators to be up to date.
    submit_pending: bool,
}

/// A piece of named state.
#[derive(Clone, Debug)]
struct Slot {
    /// `None` for derived state that isn't computed yet.
    value: Option<Value>,
    /// Bumped whenever the value changes.
    version: u64,
}

struct Derived {
    name: String,
    compute: DeriveFn,
    /// The named state the computation may read.
    deps: Vec<String>,
    /// The indices of the derived state among `deps`.
    derived_deps: Vec<usize>,
    /// What the last finished computation read; `None` before the first one
    /// finishes.
    reads: Option<Reads>,
    /// Whether a computation is in flight.
    running: bool,
    /// Whether the value may not match the current inputs; see
    /// [`Model::is_outdated`].
    outdated: bool,
    /// Whether it's a validator. Its named state is then `""` until errors
    /// are shown, and the error it found is kept in `error`.
    validator: bool,
    /// The last error a validator found; empty if none.
    error: String,
}

struct ActionSlot {
    id: String,
    run: ActionFn,
    /// The named state the action may read.
    deps: Vec<String>,
    /// The indices of the derived state among `deps`.
    derived_deps: Vec<usize>,
    /// Presses that haven't started running it yet.
    pending: usize,
    /// Whether it's running.
    running: bool,
}

/// What a computation read, and the values it saw.
#[derive(Clone, Debug)]
struct Reads {
    fields: Vec<(Arc<str>, Field)>,
    /// Named state and the version seen. Versions rather than values, so a
    /// value that never equals itself (NaN) can't cause endless reruns.
    state: Vec<(String, u64)>,
}

/// How the user finished with an island. The island stays usable; it's up to
/// the host whether to close it.
#[derive(Clone, Debug)]
pub enum Outcome {
    /// The user pressed a [`submit_button`](crate::widget::submit_button),
    /// and the validators passed.
    Submitted(Fields),
    /// The user pressed a [`cancel_button`](crate::widget::cancel_button).
    Cancelled,
    /// The user pressed a [`button`](crate::widget::button) with this id.
    Pressed(Arc<str>, Fields),
}

impl Model {
    /// `derived` must be in dependency order, with all dependencies existing,
    /// and so must the dependencies of `actions`.
    pub(crate) fn new(
        root: Widget,
        fields: Fields,
        state: HashMap<String, Value>,
        derived: Vec<DeriveSpec>,
        actions: Vec<ActionSpec>,
    ) -> (Self, Vec<Job>) {
        let settable = Arc::new(state.keys().cloned().collect());
        let mut state: HashMap<String, Slot> = state
            .into_iter()
            .map(|(name, value)| {
                let slot = Slot {
                    value: Some(value),
                    version: 0,
                };
                (name, slot)
            })
            .collect();
        let index: HashMap<String, usize> = derived
            .iter()
            .enumerate()
            .map(|(i, spec)| (spec.name.clone(), i))
            .collect();
        let derived_deps = |deps: &[String]| -> Vec<usize> {
            (deps.iter())
                .filter_map(|d| index.get(d))
                .copied()
                .collect()
        };
        let actions = actions
            .into_iter()
            .map(|spec| ActionSlot {
                derived_deps: derived_deps(&spec.deps),
                id: spec.id,
                run: spec.run,
                deps: spec.deps,
                pending: 0,
                running: false,
            })
            .collect();
        let derived = derived
            .into_iter()
            .map(|spec| {
                // A validator shows nothing until errors are shown.
                let slot = Slot {
                    value: spec.validator.then(|| Value::Text(String::new())),
                    version: 0,
                };
                state.insert(spec.name.clone(), slot);
                Derived {
                    derived_deps: derived_deps(&spec.deps),
                    name: spec.name,
                    compute: spec.compute,
                    deps: spec.deps,
                    reads: None,
                    running: false,
                    outdated: true,
                    validator: spec.validator,
                    error: String::new(),
                }
            })
            .collect();
        let mut model = Model {
            root: Arc::new(root),
            fields,
            state,
            derived,
            actions,
            settable,
            errors_shown: false,
            submit_pending: false,
        };
        let jobs = model.schedule();
        (model, jobs)
    }

    /// The (fixed) widget tree.
    pub fn root(&self) -> &Widget {
        &self.root
    }

    /// The current values of all form elements.
    pub fn fields(&self) -> &Fields {
        &self.fields
    }

    /// The current value of named state; `None` if it isn't computed yet.
    pub fn state(&self, name: &str) -> Option<&Value> {
        self.state.get(name)?.value.as_ref()
    }

    /// Whether named state is derived and its value may not match the current
    /// inputs: it's being recomputed, waiting for derived state it depends on,
    /// or not computed yet. A backend may show such values differently.
    pub fn is_outdated(&self, name: &str) -> bool {
        self.derived.iter().any(|d| d.name == name && d.outdated)
    }

    /// A snapshot of the island that can't change. Outdated values stay
    /// marked as outdated.
    pub fn freeze(&self) -> FrozenIsland {
        let state = (self.state.iter())
            .filter_map(|(name, slot)| Some((name.clone(), slot.value.clone()?)))
            .collect();
        let outdated = (self.derived.iter())
            .filter(|d| d.outdated)
            .map(|d| d.name.clone())
            .collect();
        FrozenIsland::new(self.root.clone(), self.fields.clone(), state, outdated)
    }

    /// Sets the contents of a text field. Returns the jobs that recompute
    /// derived state.
    #[track_caller]
    pub fn set_text(&mut self, id: &str, value: String) -> Vec<Job> {
        self.set_field(id, Field::Text(value))
    }

    /// Checks or unchecks a checkbox. Returns the jobs that recompute derived
    /// state.
    #[track_caller]
    pub fn set_checked(&mut self, id: &str, value: bool) -> Vec<Job> {
        self.set_field(id, Field::Bool(value))
    }

    /// Changes a field. Drops a pending submit (see [`Model::submit`]).
    #[track_caller]
    fn set_field(&mut self, id: &str, value: Field) -> Vec<Job> {
        match self.fields.0.get_mut(id) {
            Some(field) if std::mem::discriminant(field) == std::mem::discriminant(&value) => {
                *field = value;
                self.submit_pending = false;
                self.schedule()
            }
            _ => {
                misuse(
                    strict(),
                    format!("no form element {id:?} of this kind"),
                    "ignoring the change",
                );
                Vec::new()
            }
        }
    }

    /// Asks to submit the island, as a
    /// [`submit_button`](crate::widget::submit_button) does. From now on,
    /// validators show their errors. Returns the jobs that recompute derived
    /// state that depends on them.
    ///
    /// The submit waits until the validators are up to date; call
    /// [`take_submitted`](Model::take_submitted) after this and after every
    /// [`finish`](Model::finish) to get the result. A change of a field
    /// drops it, since the user didn't submit the new values.
    pub fn submit(&mut self) -> Vec<Job> {
        self.submit_pending = true;
        if !self.errors_shown {
            self.errors_shown = true;
            for i in 0..self.derived.len() {
                let derived = &self.derived[i];
                if derived.validator {
                    let (name, error) = (derived.name.clone(), derived.error.clone());
                    self.store(name, Value::Text(error));
                }
            }
        }
        self.schedule()
    }

    /// The fields of a submit ([`submit`](Model::submit)) that went through:
    /// once the validators are up to date, if none of them found an error.
    /// Returns them only once. `None` if the submit still waits for the
    /// validators, if it was dropped, or if there was none.
    pub fn take_submitted(&mut self) -> Option<Fields> {
        let waiting = self.derived.iter().any(|d| d.validator && d.outdated);
        if !self.submit_pending || waiting {
            return None;
        }
        self.submit_pending = false;
        self.validation_errors()
            .is_empty()
            .then(|| self.fields.clone())
    }

    /// The errors the validators found last, even if they aren't shown
    /// yet. Empty if all of them passed.
    pub fn validation_errors(&self) -> Vec<&str> {
        (self.derived.iter())
            .filter(|d| d.validator && !d.error.is_empty())
            .map(|d| d.error.as_str())
            .collect()
    }

    /// Runs the action with this id, as an
    /// [`action_button`](crate::widget::action_button) does. Returns the
    /// job that runs it, unless it has to wait: until it's done running
    /// for an earlier press, or until the derived state it reads is up to
    /// date. Its changes are applied when the job
    /// [finishes](Model::finish).
    ///
    /// An unknown id panics in [strict mode](crate#strict-mode); otherwise
    /// it logs a warning and does nothing.
    #[track_caller]
    pub fn run_action(&mut self, id: &str) -> Vec<Job> {
        match self.actions.iter_mut().find(|a| a.id == id) {
            Some(action) => {
                action.pending += 1;
                self.schedule()
            }
            None => {
                misuse(strict(), format!("no action {id:?}"), "ignoring the press");
                Vec::new()
            }
        }
    }

    /// Applies an update sent through a [`StateHandle`](crate::StateHandle).
    /// Returns the jobs that recompute derived state.
    pub fn apply(&mut self, StateUpdate { name, value }: StateUpdate) -> Vec<Job> {
        // `StateHandle::set` only sends names with an initial value, and
        // `IslandBuilder::check` rejects names that are also derived.
        self.store(name, value);
        self.schedule()
    }

    /// Stores the result of a [`Job`]. Returns the jobs that recompute
    /// derived state: this one again if what it read has changed since it
    /// started, and the derived state that depends on it.
    ///
    /// The result is stored even if it's outdated, since it's still newer
    /// than the value it replaces. An action's changes are applied the same
    /// way, even if the user has changed the fields since it started; a
    /// change of a field drops a pending submit, as when the user changes
    /// it.
    pub fn finish(&mut self, result: JobResult) -> Vec<Job> {
        let result = match result.0 {
            Finished::Derived(result) => result,
            Finished::Action { index, changes } => {
                self.actions[index].running = false;
                for (id, value) in changes.fields {
                    // Changes checked the id and kind against the same tree.
                    if let Some(field) = self.fields.0.get_mut(&id)
                        && *field != value
                    {
                        *field = value;
                        self.submit_pending = false;
                    }
                }
                for (name, value) in changes.state {
                    self.store(name, value);
                }
                return self.schedule();
            }
        };
        let derived = &mut self.derived[result.index];
        derived.running = false;
        derived.reads = Some(result.reads);
        if let Some(mut value) = result.value {
            if derived.validator {
                derived.error = value.to_string();
                if !self.errors_shown {
                    value = Value::Text(String::new());
                }
            }
            let name = derived.name.clone();
            self.store(name, value);
        }
        self.schedule()
    }

    fn store(&mut self, name: String, value: Value) {
        let slot = self.state.entry(name).or_insert(Slot {
            value: None,
            version: 0,
        });
        if slot.value.as_ref() != Some(&value) {
            slot.value = Some(value);
            slot.version += 1;
        }
    }

    /// Starts every computation that is due: its inputs changed since it last
    /// ran, it isn't already running, and the derived state it depends on is
    /// up to date.
    fn schedule(&mut self) -> Vec<Job> {
        let mut jobs = Vec::new();
        for index in 0..self.derived.len() {
            let derived = &self.derived[index];
            let waiting = derived
                .derived_deps
                .iter()
                .any(|&d| self.derived[d].outdated);
            let stale = self.is_stale(derived);
            if stale && !waiting && !derived.running {
                jobs.push(self.start(index));
            }
            let derived = &mut self.derived[index];
            derived.outdated = stale || waiting || derived.running;
        }
        for index in 0..self.actions.len() {
            let action = &self.actions[index];
            let waiting = (action.derived_deps.iter()).any(|&d| self.derived[d].outdated);
            if action.pending > 0 && !action.running && !waiting {
                jobs.push(self.start_action(index));
            }
        }
        jobs
    }

    fn start_action(&mut self, index: usize) -> Job {
        let action = &mut self.actions[index];
        action.pending -= 1;
        action.running = true;
        Job(Work::Action {
            index,
            run: action.run.clone(),
            fields: self.fields.clone(),
            state: snapshot(&self.state, &action.deps),
            settable: self.settable.clone(),
            strict: strict(),
        })
    }

    /// Whether anything the last computation read has changed since.
    fn is_stale(&self, derived: &Derived) -> bool {
        let Some(reads) = &derived.reads else {
            return true;
        };
        let field_changed = reads
            .fields
            .iter()
            .any(|(id, value)| self.fields.0.get(id) != Some(value));
        let state_changed = reads
            .state
            .iter()
            .any(|(name, version)| self.state[name].version != *version);
        field_changed || state_changed
    }

    fn start(&mut self, index: usize) -> Job {
        let derived = &mut self.derived[index];
        derived.running = true;
        Job(Work::Derive {
            index,
            compute: derived.compute.clone(),
            fields: self.fields.clone(),
            state: snapshot(&self.state, &derived.deps),
            strict: strict(),
        })
    }
}

/// The named state in `deps`, for a job.
fn snapshot(state: &HashMap<String, Slot>, deps: &[String]) -> HashMap<String, Slot> {
    (deps.iter())
        .map(|name| (name.clone(), state[name].clone()))
        .collect()
}

impl Contents for Model {
    fn root(&self) -> &Widget {
        self.root()
    }

    fn fields(&self) -> &Fields {
        self.fields()
    }

    fn state(&self, name: &str) -> Option<&Value> {
        self.state(name)
    }

    fn is_outdated(&self, name: &str) -> bool {
        self.is_outdated(name)
    }
}

/// Computes one derived value, or runs an action, from a snapshot of its
/// inputs. It may be slow, so run it off the UI thread, and pass the result
/// to [`Model::finish`].
///
/// Every job must be run and finished: until it is, its derived value won't be
/// computed again, or its action run again.
pub struct Job(Work);

enum Work {
    Derive {
        index: usize,
        compute: DeriveFn,
        fields: Fields,
        /// The named state the computation may read.
        state: HashMap<String, Slot>,
        strict: bool,
    },
    Action {
        index: usize,
        run: ActionFn,
        fields: Fields,
        /// The named state the action may read.
        state: HashMap<String, Slot>,
        settable: Arc<HashSet<String>>,
        strict: bool,
    },
}

impl Job {
    /// Does the computation. Call it off the UI thread.
    ///
    /// If a computation panics, the derived value keeps its old value until
    /// one of the inputs it read before panicking changes. If an action
    /// panics, none of its changes are applied.
    pub fn run(self) -> JobResult {
        JobResult(match self.0 {
            Work::Derive {
                index,
                compute,
                fields,
                state,
                strict,
            } => {
                let inputs = Inputs::new(&fields, &state, strict);
                let value = panic::catch_unwind(AssertUnwindSafe(|| compute(&inputs))).ok();
                Finished::Derived(Derivation {
                    index,
                    value,
                    reads: inputs.into_reads(),
                })
            }
            Work::Action {
                index,
                run,
                fields,
                state,
                settable,
                strict,
            } => {
                let inputs = Inputs::new(&fields, &state, strict);
                let mut changes = Changes {
                    fields: &fields,
                    settable: &settable,
                    strict,
                    changed: Changed::default(),
                };
                let ran = panic::catch_unwind(AssertUnwindSafe(|| run(&inputs, &mut changes)));
                Finished::Action {
                    index,
                    changes: ran.map(|()| changes.changed).unwrap_or_default(),
                }
            }
        })
    }
}

/// The result of a [`Job`].
#[derive(Clone, Debug)]
pub struct JobResult(Finished);

#[derive(Clone, Debug)]
enum Finished {
    Derived(Derivation),
    Action { index: usize, changes: Changed },
}

#[derive(Clone, Debug)]
struct Derivation {
    index: usize,
    /// `None` if the computation panicked.
    value: Option<Value>,
    reads: Reads,
}

/// What an action changes: fields and named state, in the order set.
#[derive(Clone, Debug, Default)]
struct Changed {
    fields: Vec<(Arc<str>, Field)>,
    state: Vec<(String, Value)>,
}

/// The changes an action ([`IslandBuilder::action`](crate::IslandBuilder::action))
/// makes, applied once it finishes. Setting something twice keeps the last
/// value.
pub struct Changes<'a> {
    /// The snapshot the action reads, to check ids against.
    fields: &'a Fields,
    settable: &'a HashSet<String>,
    strict: bool,
    changed: Changed,
}

impl Changes<'_> {
    /// Sets the contents of the text field or text area `id`. If there's no
    /// such field, this panics in [strict mode](crate#strict-mode), or prints
    /// a warning and does nothing.
    #[track_caller]
    pub fn set_text(&mut self, id: &str, text: impl Into<String>) {
        self.set_field(id, Field::Text(text.into()));
    }

    /// Checks or unchecks the checkbox `id`. If there's no such checkbox,
    /// this panics in [strict mode](crate#strict-mode), or prints a warning
    /// and does nothing.
    #[track_caller]
    pub fn set_checked(&mut self, id: &str, checked: bool) {
        self.set_field(id, Field::Bool(checked));
    }

    /// Sets named state. As with a [`StateHandle`](crate::StateHandle), it
    /// must have an initial value
    /// ([`IslandBuilder::set_state`](crate::IslandBuilder::set_state));
    /// otherwise this panics in [strict mode](crate#strict-mode), or prints
    /// a warning and does nothing.
    #[track_caller]
    pub fn set_state(&mut self, name: &str, value: impl Into<Value>) {
        if !self.settable.contains(name) {
            misuse(
                self.strict,
                format!(
                    "named state {name:?} has no initial value, so an action can't set it \
                     (give it one with IslandBuilder::set_state)"
                ),
                "ignoring the change",
            );
            return;
        }
        self.changed.state.push((name.to_owned(), value.into()));
    }

    #[track_caller]
    fn set_field(&mut self, id: &str, value: Field) {
        match self.fields.0.get_key_value(id) {
            Some((id, field))
                if std::mem::discriminant(field) == std::mem::discriminant(&value) =>
            {
                self.changed.fields.push((id.clone(), value));
            }
            _ => misuse(
                self.strict,
                format!("no form element {id:?} of this kind"),
                "ignoring the change",
            ),
        }
    }
}

/// What a derived value is computed from, or what an action reads: a
/// snapshot of the field values and of the named state it declared as
/// dependencies. Everything read through it
/// is recorded, so the value is recomputed only when that changes.
pub struct Inputs<'a> {
    fields: &'a Fields,
    state: &'a HashMap<String, Slot>,
    strict: bool,
    read_fields: RefCell<HashSet<Arc<str>>>,
    read_state: RefCell<HashSet<String>>,
}

impl<'a> Inputs<'a> {
    fn new(fields: &'a Fields, state: &'a HashMap<String, Slot>, strict: bool) -> Self {
        Inputs {
            fields,
            state,
            strict,
            read_fields: RefCell::default(),
            read_state: RefCell::default(),
        }
    }

    /// The contents of a text field, or `""` if there is no such field.
    pub fn text(&self, id: &str) -> &'a str {
        self.read_field(id);
        self.fields.text(id)
    }

    /// Whether a checkbox is checked, or `false` if there is no such checkbox.
    pub fn checked(&self, id: &str) -> bool {
        self.read_field(id);
        self.fields.checked(id)
    }

    /// The value of named state; `None` if it isn't computed yet. The name
    /// must be declared as a dependency
    /// ([`IslandBuilder::derive_with_state`](crate::IslandBuilder::derive_with_state)
    /// or [`action_with_state`](crate::IslandBuilder::action_with_state));
    /// otherwise this panics in [strict mode](crate#strict-mode), or prints a
    /// warning and returns `None`.
    #[track_caller]
    pub fn state(&self, name: &str) -> Option<&'a Value> {
        match self.state.get_key_value(name) {
            Some((name, slot)) => {
                self.read_state.borrow_mut().insert(name.clone());
                slot.value.as_ref()
            }
            None => {
                misuse(
                    self.strict,
                    format!(
                        "named state {name:?} isn't a declared dependency of this derived \
                         state or action (declare it with IslandBuilder::derive_with_state, \
                         validate_with_state or action_with_state)"
                    ),
                    "returning None",
                );
                None
            }
        }
    }

    fn read_field(&self, id: &str) {
        // A field that doesn't exist never changes, so it needn't be recorded.
        if let Some((id, _)) = self.fields.0.get_key_value(id) {
            self.read_fields.borrow_mut().insert(id.clone());
        }
    }

    fn into_reads(self) -> Reads {
        Reads {
            fields: (self.read_fields.into_inner().into_iter())
                .map(|id| {
                    let value = self.fields.0[&id].clone();
                    (id, value)
                })
                .collect(),
            state: (self.read_state.into_inner().into_iter())
                .map(|name| {
                    let version = self.state[&name].version;
                    (name, version)
                })
                .collect(),
        }
    }
}

/// A change of named state sent through a [`StateHandle`](crate::StateHandle).
#[derive(Clone, Debug, PartialEq)]
pub struct StateUpdate {
    pub(crate) name: String,
    pub(crate) value: Value,
}

/// The stream of [`StateUpdate`]s for one island. It ends when all
/// [`StateHandle`](crate::StateHandle)s (and the builder) are dropped.
pub struct Updates(pub(crate) mpsc::UnboundedReceiver<StateUpdate>);

impl Stream for Updates {
    type Item = StateUpdate;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<StateUpdate>> {
        Pin::new(&mut self.get_mut().0).poll_next(cx)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::IslandBuilder;
    use crate::widget::*;

    fn build(builder: IslandBuilder) -> (Model, Vec<Job>) {
        let built = builder.build().unwrap();
        (built.model, built.jobs)
    }

    /// Runs the jobs, and the ones they lead to, until there are none left.
    fn settle(model: &mut Model, mut jobs: Vec<Job>) {
        while let Some(job) = jobs.pop() {
            jobs.extend(model.finish(job.run()));
        }
    }

    fn text(model: &Model, name: &str) -> String {
        model.state(name).map(Value::to_string).unwrap_or_default()
    }

    /// A counter, and a function that increments it.
    fn counter() -> (Arc<AtomicUsize>, impl Fn() + Send + Sync + 'static) {
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        (count, move || {
            c.fetch_add(1, Ordering::Relaxed);
        })
    }

    #[test]
    fn recomputes_only_when_a_read_field_changes() {
        let mut b = IslandBuilder::new(column([text_field("a", ""), text_field("b", "")]));
        b.derive("x", |i| i.text("a").into());
        let (mut model, jobs) = build(b);
        settle(&mut model, jobs);

        assert!(model.set_text("b", "1".into()).is_empty());
        let jobs = model.set_text("a", "1".into());
        assert_eq!(jobs.len(), 1);
        settle(&mut model, jobs);
        assert_eq!(text(&model, "x"), "1");
    }

    #[test]
    fn conditional_reads_are_tracked() {
        let mut b = IslandBuilder::new(column([checkbox("c", ""), text_field("t", "")]));
        b.derive("x", |i| match i.checked("c") {
            true => i.text("t").into(),
            false => "off".into(),
        });
        let (mut model, jobs) = build(b);
        settle(&mut model, jobs);

        assert!(model.set_text("t", "1".into()).is_empty());
        let jobs = model.set_checked("c", true);
        settle(&mut model, jobs);
        assert_eq!(text(&model, "x"), "1");
        let jobs = model.set_text("t", "2".into());
        settle(&mut model, jobs);
        assert_eq!(text(&model, "x"), "2");
    }

    #[test]
    fn one_computation_in_flight_and_rerun_when_outdated() {
        let (count, inc) = counter();
        let mut b = IslandBuilder::new(text_field("a", ""));
        b.derive("x", move |i| {
            inc();
            i.text("a").into()
        });
        let (mut model, jobs) = build(b);
        settle(&mut model, jobs);

        let mut jobs = model.set_text("a", "1".into());
        let job = jobs.pop().unwrap();
        assert!(model.set_text("a", "12".into()).is_empty());
        assert!(model.set_text("a", "123".into()).is_empty());
        let result = job.run();

        assert!(model.is_outdated("x"));

        // The outdated result is shown while the rerun is in progress.
        let jobs = model.finish(result);
        assert_eq!(text(&model, "x"), "1");
        assert!(model.is_outdated("x"));
        assert_eq!(jobs.len(), 1);
        settle(&mut model, jobs);
        assert_eq!(text(&model, "x"), "123");
        assert!(!model.is_outdated("x"));
        assert_eq!(count.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn no_rerun_when_the_value_is_back_to_what_was_read() {
        let mut b = IslandBuilder::new(text_field("a", ""));
        b.derive("x", |i| i.text("a").into());
        let (mut model, jobs) = build(b);
        settle(&mut model, jobs);

        let job = model.set_text("a", "1".into()).pop().unwrap();
        model.set_text("a", "12".into());
        model.set_text("a", "1".into());
        assert!(model.finish(job.run()).is_empty());
    }

    #[test]
    fn derived_state_waits_for_its_dependencies() {
        let (count, inc) = counter();
        let mut b = IslandBuilder::new(column([text_field("a", ""), checkbox("c", "")]));
        b.derive("len", |i| (i.text("a").len() as f64).into());
        b.derive_with_state("summary", &["len"], move |i| {
            inc();
            format!("{} chars", i.state("len").unwrap()).into()
        });
        let (mut model, jobs) = build(b);

        // Only "len" starts; "summary" needs its result first.
        assert_eq!(jobs.len(), 1);
        assert!(model.is_outdated("summary"));
        settle(&mut model, jobs);
        assert_eq!(text(&model, "summary"), "0 chars");
        assert!(!model.is_outdated("summary"));

        let jobs = model.set_text("a", "ab".into());
        assert_eq!(jobs.len(), 1);
        // Waiting for "len" counts as outdated too.
        assert!(model.is_outdated("summary"));
        settle(&mut model, jobs);
        assert_eq!(text(&model, "summary"), "2 chars");

        // Same length, so "summary" isn't recomputed.
        let jobs = model.set_text("a", "cd".into());
        settle(&mut model, jobs);
        assert_eq!(count.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn frozen_snapshot_does_not_change() {
        let mut b = IslandBuilder::new(column([text_field("a", ""), label(state("x"))]));
        b.derive("x", |i| i.text("a").into());
        let (mut model, jobs) = build(b);
        settle(&mut model, jobs);
        let jobs = model.set_text("a", "1".into());
        settle(&mut model, jobs);

        let frozen = model.freeze();
        let jobs = model.set_text("a", "2".into());
        assert!(model.freeze().is_outdated("x"));
        settle(&mut model, jobs);
        assert_eq!(frozen.fields().text("a"), "1");
        assert_eq!(frozen.state("x"), Some(&"1".into()));
        assert!(!frozen.is_outdated("x"));
    }

    #[test]
    fn derived_state_can_depend_on_set_state() {
        let mut b = IslandBuilder::new(column([]));
        b.set_state("s", "a");
        b.derive_with_state("x", &["s"], |i| {
            format!("<{}>", i.state("s").unwrap()).into()
        });
        let (mut model, jobs) = build(b);
        settle(&mut model, jobs);
        assert_eq!(text(&model, "x"), "<a>");

        let update = StateUpdate {
            name: "s".into(),
            value: "b".into(),
        };
        let jobs = model.apply(update);
        settle(&mut model, jobs);
        assert_eq!(text(&model, "x"), "<b>");
    }

    #[test]
    fn panicking_computation_is_retried_when_inputs_change() {
        let mut b = IslandBuilder::new(text_field("a", ""));
        b.derive("x", |i| match i.text("a") {
            "boom" => panic!("test panic"),
            a => a.into(),
        });
        let (mut model, jobs) = build(b);
        settle(&mut model, jobs);

        let jobs = model.set_text("a", "boom".into());
        settle(&mut model, jobs);
        assert_eq!(text(&model, "x"), "");
        let jobs = model.set_text("a", "ok".into());
        settle(&mut model, jobs);
        assert_eq!(text(&model, "x"), "ok");
    }

    /// An island with a text field `name` that must not be empty.
    fn name_required() -> IslandBuilder {
        let mut b = IslandBuilder::new(column([text_field("name", ""), label(state("err"))]));
        b.validate("err", |i| {
            i.text("name").is_empty().then(|| "Enter a name".into())
        });
        b
    }

    #[test]
    fn errors_are_shown_once_submitting_fails() {
        let (mut model, jobs) = build(name_required());
        settle(&mut model, jobs);
        assert_eq!(text(&model, "err"), "");
        assert_eq!(model.validation_errors(), ["Enter a name"]);
        assert!(model.take_submitted().is_none());

        let jobs = model.submit();
        settle(&mut model, jobs);
        assert!(model.take_submitted().is_none());
        assert_eq!(text(&model, "err"), "Enter a name");

        // From now on, the error follows the fields.
        let jobs = model.set_text("name", "Ada".into());
        settle(&mut model, jobs);
        assert_eq!(text(&model, "err"), "");
        assert!(
            model.take_submitted().is_none(),
            "the failed submit is used up"
        );
        let jobs = model.set_text("name", "".into());
        settle(&mut model, jobs);
        assert_eq!(text(&model, "err"), "Enter a name");

        let jobs = model.set_text("name", "Ada".into());
        settle(&mut model, jobs);
        assert!(model.submit().is_empty());
        assert_eq!(model.take_submitted().unwrap().text("name"), "Ada");
        assert!(model.take_submitted().is_none());
    }

    #[test]
    fn submit_waits_for_validators() {
        let (mut model, jobs) = build(name_required());
        settle(&mut model, jobs);
        let mut jobs = model.set_text("name", "Ada".into());
        assert!(model.submit().is_empty());
        assert!(model.take_submitted().is_none());
        let job = jobs.pop().unwrap();
        assert!(model.finish(job.run()).is_empty());
        assert_eq!(model.take_submitted().unwrap().text("name"), "Ada");

        // A field change drops a waiting submit.
        let jobs = model.set_text("name", "Bob".into());
        model.submit();
        assert!(model.set_text("name", "Cy".into()).is_empty());
        settle(&mut model, jobs);
        assert!(model.take_submitted().is_none());
    }

    #[test]
    fn submits_without_validators() {
        let (mut model, jobs) = build(IslandBuilder::new(text_field("a", "")));
        settle(&mut model, jobs);
        assert!(model.submit().is_empty());
        assert!(model.take_submitted().is_some());
    }

    #[test]
    fn several_validators_must_all_pass() {
        let mut b = IslandBuilder::new(column([text_field("a", ""), checkbox("c", "")]));
        b.validate("a_err", |i| i.text("a").is_empty().then(|| "a".into()));
        b.validate("c_err", |i| (!i.checked("c")).then(|| "c".into()));
        let (mut model, jobs) = build(b);
        settle(&mut model, jobs);
        assert_eq!(model.validation_errors(), ["a", "c"]);

        let jobs = model.set_text("a", "x".into());
        settle(&mut model, jobs);
        let jobs = model.submit();
        settle(&mut model, jobs);
        assert!(model.take_submitted().is_none());
        assert_eq!(text(&model, "c_err"), "c");

        let jobs = model.set_checked("c", true);
        settle(&mut model, jobs);
        let jobs = model.submit();
        settle(&mut model, jobs);
        assert!(model.take_submitted().is_some());
    }

    #[test]
    fn validators_can_use_state_and_be_used() {
        let mut b = IslandBuilder::new(text_field("a", ""));
        b.derive("len", |i| (i.text("a").len() as f64).into());
        b.validate_with_state("err", &["len"], |i| {
            (i.state("len") != Some(&3.0.into())).then(|| "Three letters".into())
        });
        b.derive_with_state("shown", &["err"], |i| {
            format!("<{}>", i.state("err").unwrap()).into()
        });
        let (mut model, jobs) = build(b);
        settle(&mut model, jobs);
        // Derived state sees what's shown, not the hidden error.
        assert_eq!(text(&model, "shown"), "<>");
        let jobs = model.submit();
        assert_eq!(jobs.len(), 1);
        settle(&mut model, jobs);
        assert_eq!(text(&model, "shown"), "<Three letters>");

        // The submit waits for "len" too.
        let jobs = model.set_text("a", "abc".into());
        model.submit();
        assert!(model.take_submitted().is_none());
        settle(&mut model, jobs);
        assert!(model.take_submitted().is_some());
        assert_eq!(text(&model, "shown"), "<>");
    }

    #[test]
    fn frozen_island_hides_errors_until_submitted() {
        let (mut model, jobs) = build(name_required());
        settle(&mut model, jobs);
        assert_eq!(model.freeze().state("err"), Some(&"".into()));
        let jobs = model.submit();
        settle(&mut model, jobs);
        assert_eq!(model.freeze().state("err"), Some(&"Enter a name".into()));
    }

    #[test]
    fn actions_change_fields_and_state() {
        let mut b = IslandBuilder::new(column([
            text_field("name", ""),
            checkbox("c", ""),
            label(state("n")),
            label(state("shout")),
            action_button("up", "Uppercase"),
        ]));
        b.set_state("n", 0.0);
        b.derive("shout", |i| format!("{}!", i.text("name")).into());
        b.action_with_state("up", &["n"], |i, c| {
            c.set_text("name", i.text("name").to_uppercase());
            c.set_checked("c", !i.checked("c"));
            let Some(Value::Number(n)) = i.state("n") else {
                panic!("n is a number");
            };
            c.set_state("n", n + 1.0);
        });
        let (mut model, jobs) = build(b);
        settle(&mut model, jobs);
        let jobs = model.set_text("name", "ada".into());
        settle(&mut model, jobs);

        let jobs = model.run_action("up");
        assert_eq!(jobs.len(), 1);
        settle(&mut model, jobs);
        assert_eq!(model.fields().text("name"), "ADA");
        assert!(model.fields().checked("c"));
        assert_eq!(text(&model, "n"), "1");
        // Derived state follows the changed fields.
        assert_eq!(text(&model, "shout"), "ADA!");
    }

    #[test]
    fn presses_while_an_action_runs_wait_for_it() {
        let (count, inc) = counter();
        let mut b = IslandBuilder::new(column([text_field("t", ""), action_button("a", "A")]));
        b.action("a", move |i, c| {
            inc();
            c.set_text("t", format!("{}x", i.text("t")));
        });
        let (mut model, jobs) = build(b);
        settle(&mut model, jobs);

        let mut jobs = model.run_action("a");
        assert!(model.run_action("a").is_empty());
        assert!(model.run_action("a").is_empty());
        let job = jobs.pop().unwrap();
        // Each press runs it once, from the fields the last run left.
        settle(&mut model, vec![job]);
        assert_eq!(model.fields().text("t"), "xxx");
        assert_eq!(count.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn actions_wait_for_the_derived_state_they_read() {
        let mut b = IslandBuilder::new(column([text_field("t", ""), action_button("a", "A")]));
        b.derive("len", |i| (i.text("t").len() as f64).into());
        b.action_with_state("a", &["len"], |i, c| {
            c.set_text("t", i.state("len").unwrap().to_string());
        });
        let (mut model, jobs) = build(b);
        settle(&mut model, jobs);

        let mut jobs = model.set_text("t", "abc".into());
        assert!(model.run_action("a").is_empty(), "waits for len");
        let job = jobs.pop().unwrap();
        let jobs = model.finish(job.run());
        settle(&mut model, jobs);
        assert_eq!(model.fields().text("t"), "3");
    }

    #[test]
    fn an_action_changing_a_field_drops_a_pending_submit() {
        let mut b = name_required();
        b.action("fill", |_, c| c.set_text("name", "Ada"));
        let (mut model, jobs) = build(b);
        settle(&mut model, jobs);

        let jobs = model.set_text("name", "Bob".into());
        let action = model.run_action("fill").pop().unwrap();
        model.submit();
        settle(&mut model, vec![action]);
        settle(&mut model, jobs);
        assert!(model.take_submitted().is_none());
        assert_eq!(model.fields().text("name"), "Ada");
    }

    #[test]
    fn a_panicking_action_changes_nothing() {
        let mut b = IslandBuilder::new(column([text_field("t", ""), action_button("a", "A")]));
        b.action("a", |_, c| {
            c.set_text("t", "changed");
            panic!("test panic");
        });
        let (mut model, jobs) = build(b);
        settle(&mut model, jobs);
        let jobs = model.run_action("a");
        settle(&mut model, jobs);
        assert_eq!(model.fields().text("t"), "");
        // It can run again.
        assert_eq!(model.run_action("a").len(), 1);
    }

    fn changes<'a>(fields: &'a Fields, settable: &'a HashSet<String>, strict: bool) -> Changes<'a> {
        Changes {
            fields,
            settable,
            strict,
            changed: Changed::default(),
        }
    }

    #[test]
    #[should_panic(expected = "has no initial value")]
    fn actions_cannot_set_derived_state_in_strict_mode() {
        let (fields, settable) = (Fields::default(), HashSet::new());
        changes(&fields, &settable, true).set_state("derived", "x");
    }

    #[test]
    fn actions_ignore_unknown_fields_and_state_otherwise() {
        let ui = IslandBuilder::new(column([checkbox("c", "")]));
        let (fields, _) = ui.check().unwrap();
        let settable = HashSet::from(["s".to_owned()]);
        let mut c = changes(&fields, &settable, false);
        c.set_text("c", "not a text field");
        c.set_checked("nope", true);
        c.set_state("derived", "x");
        c.set_checked("c", true);
        c.set_state("s", "x");
        assert_eq!(c.changed.fields, [("c".into(), Field::Bool(true))]);
        assert_eq!(c.changed.state, [("s".to_owned(), "x".into())]);
    }

    #[test]
    #[should_panic(expected = "isn't a declared dependency")]
    fn reading_undeclared_state_panics_in_strict_mode() {
        let (fields, state) = (Fields::default(), HashMap::new());
        Inputs::new(&fields, &state, true).state("s");
    }

    #[test]
    fn reading_undeclared_state_returns_none_otherwise() {
        let (fields, state) = (Fields::default(), HashMap::new());
        assert_eq!(Inputs::new(&fields, &state, false).state("s"), None);
    }
}
