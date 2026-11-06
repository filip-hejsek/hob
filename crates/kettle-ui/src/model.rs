//! [`Model`]: the state of a running island, independent of any GUI toolkit.
//!
//! A backend (such as `kettle-ui-iced`) renders [`Model::root`], feeds user
//! input back through the `set_*` methods, runs the [`Job`]s they return off
//! the UI thread, and delivers [`StateUpdate`]s from [`Updates`].

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::panic::{self, AssertUnwindSafe};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures_channel::mpsc;
use futures_core::Stream;

use crate::widget::Widget;
use crate::{Contents, DeriveFn, DeriveSpec, Field, Fields, FrozenIsland, Value, misuse, strict};

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
    /// The user pressed a [`submit_button`](crate::widget::submit_button).
    Submitted(Fields),
    /// The user pressed a [`cancel_button`](crate::widget::cancel_button).
    Cancelled,
}

impl Model {
    /// `derived` must be in dependency order, with all dependencies existing.
    pub(crate) fn new(
        root: Widget,
        fields: Fields,
        state: HashMap<String, Value>,
        derived: Vec<DeriveSpec>,
    ) -> (Self, Vec<Job>) {
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
        let derived = derived
            .into_iter()
            .map(|spec| {
                let slot = Slot {
                    value: None,
                    version: 0,
                };
                state.insert(spec.name.clone(), slot);
                Derived {
                    derived_deps: spec
                        .deps
                        .iter()
                        .filter_map(|d| index.get(d))
                        .copied()
                        .collect(),
                    name: spec.name,
                    compute: spec.compute,
                    deps: spec.deps,
                    reads: None,
                    running: false,
                    outdated: true,
                }
            })
            .collect();
        let mut model = Model {
            root: Arc::new(root),
            fields,
            state,
            derived,
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

    #[track_caller]
    fn set_field(&mut self, id: &str, value: Field) -> Vec<Job> {
        match self.fields.0.get_mut(id) {
            Some(field) if std::mem::discriminant(field) == std::mem::discriminant(&value) => {
                *field = value;
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
    /// than the value it replaces.
    pub fn finish(&mut self, result: JobResult) -> Vec<Job> {
        let derived = &mut self.derived[result.index];
        derived.running = false;
        derived.reads = Some(result.reads);
        if let Some(value) = result.value {
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
        jobs
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
        Job {
            index,
            compute: derived.compute.clone(),
            fields: self.fields.clone(),
            state: derived
                .deps
                .iter()
                .map(|name| (name.clone(), self.state[name].clone()))
                .collect(),
            strict: strict(),
        }
    }
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

/// Computes one derived value from a snapshot of its inputs. It may be slow,
/// so run it off the UI thread, and pass the result to [`Model::finish`].
///
/// Every job must be run and finished: until it is, its derived value won't be
/// computed again.
pub struct Job {
    index: usize,
    compute: DeriveFn,
    fields: Fields,
    /// The named state the computation may read.
    state: HashMap<String, Slot>,
    strict: bool,
}

impl Job {
    /// Does the computation. Call it off the UI thread.
    ///
    /// If the computation panics, the derived value keeps its old value until
    /// one of the inputs it read before panicking changes.
    pub fn run(self) -> JobResult {
        let inputs = Inputs::new(&self.fields, &self.state, self.strict);
        let value = panic::catch_unwind(AssertUnwindSafe(|| (self.compute)(&inputs))).ok();
        JobResult {
            index: self.index,
            value,
            reads: inputs.into_reads(),
        }
    }
}

/// The result of a [`Job`].
#[derive(Clone, Debug)]
pub struct JobResult {
    index: usize,
    /// `None` if the computation panicked.
    value: Option<Value>,
    reads: Reads,
}

/// What a derived value is computed from: a snapshot of the field values and
/// of the named state it declared as dependencies. Everything read through it
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
    /// ([`IslandBuilder::derive_with_state`](crate::IslandBuilder::derive_with_state));
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
                         state (declare it with IslandBuilder::derive_with_state)"
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
