//! [`Model`]: the state of a running island, independent of any GUI toolkit.
//!
//! A backend (such as `kettle-ui-iced`) renders [`Model::root`], feeds user
//! input back through the `set_*` methods, runs the [`Job`]s they return off
//! the UI thread, and delivers [`StateUpdate`]s from [`Updates`].

use std::collections::HashMap;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_channel::mpsc;
use futures_core::Stream;

use crate::widget::Widget;
use crate::{DeriveFn, Field, Fields, Value, misuse, strict};

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
    root: Widget,
    fields: Fields,
    state: HashMap<String, Value>,
    derived: Vec<Derived>,
}

struct Derived {
    name: String,
    compute: DeriveFn,
    /// Bumped on every recompute, so stale results can be recognised.
    generation: u64,
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
    pub(crate) fn new(
        root: Widget,
        fields: Fields,
        state: HashMap<String, Value>,
        derived: Vec<(String, DeriveFn)>,
    ) -> (Self, Vec<Job>) {
        let mut model = Model {
            root,
            fields,
            state,
            derived: derived
                .into_iter()
                .map(|(name, compute)| Derived {
                    name,
                    compute,
                    generation: 0,
                })
                .collect(),
        };
        let jobs = model.recompute();
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
        self.state.get(name)
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
                self.recompute()
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
    pub fn apply(&mut self, StateUpdate { name, value }: StateUpdate) {
        // `StateHandle::set` only sends names with an initial value, and
        // `IslandBuilder::check` rejects names that are also derived.
        self.state.insert(name, value);
    }

    /// Stores the result of a [`Job`], unless a newer job has been started
    /// for the same state since.
    pub fn finish(&mut self, result: JobResult) {
        let derived = &self.derived[result.index];
        if result.generation == derived.generation {
            self.state.insert(derived.name.clone(), result.value);
        }
    }

    /// Starts recomputing all derived state from the current fields.
    fn recompute(&mut self) -> Vec<Job> {
        self.derived
            .iter_mut()
            .enumerate()
            .map(|(index, derived)| {
                derived.generation += 1;
                Job {
                    index,
                    generation: derived.generation,
                    compute: derived.compute.clone(),
                    fields: self.fields.clone(),
                }
            })
            .collect()
    }
}

/// Computes one derived value from a snapshot of the fields. It may be slow,
/// so run it off the UI thread, and pass the result to [`Model::finish`].
pub struct Job {
    index: usize,
    generation: u64,
    compute: DeriveFn,
    fields: Fields,
}

impl Job {
    /// Does the computation. Call it off the UI thread.
    pub fn run(self) -> JobResult {
        JobResult {
            index: self.index,
            generation: self.generation,
            value: (self.compute)(&self.fields),
        }
    }
}

/// The result of a [`Job`].
#[derive(Clone, Debug)]
pub struct JobResult {
    index: usize,
    generation: u64,
    value: Value,
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
