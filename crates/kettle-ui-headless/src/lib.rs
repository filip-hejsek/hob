//! A headless backend for kettle-ui, for tests and for driving islands
//! programmatically (e.g. by an agent).
//!
//! An [`Island`] acts like a user would: it types into text fields, toggles
//! checkboxes, picks suggestions and presses buttons, all by the ids and
//! labels a user would see. It shows the island as text ([`Island`]
//! implements [`Display`](fmt::Display)).
//!
//! Derived state is computed on the calling thread as part of each action, so
//! after an action returns, everything derived from the fields is up to date.
//! Updates from [`StateHandle`](kettle_ui::StateHandle)s arrive from other
//! threads: apply them with [`Island::poll`], or wait for them with
//! [`Island::wait_until`].
//!
//! ```
//! use kettle_ui::IslandBuilder;
//! use kettle_ui::widget::*;
//! use kettle_ui_headless::Island;
//!
//! let mut builder = IslandBuilder::new(column([
//!     text_field("name", "Your name"),
//!     label(state("greeting")),
//!     submit_button("OK"),
//! ]));
//! builder.derive("greeting", |f| format!("Hello, {}!", f.text("name")).into());
//! let mut island = Island::new(builder).unwrap();
//!
//! island.type_text("name", "Ada").unwrap();
//! assert_eq!(
//!     island.to_string(),
//!     "column\n  text_field name: \"Ada\"\n  label \"Hello, Ada!\"\n  button \"OK\"\n",
//! );
//! ```

use std::fmt::{self, Write};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};
use std::time::{Duration, Instant};

use futures_core::Stream;
use kettle_ui::widget::{Action, Kind, Text, Widget};
use kettle_ui::{Built, Error, Fields, IslandBuilder, Job, Model, Outcome, Updates, Value};

/// Why an action on an [`Island`] couldn't be done.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ActionError {
    /// There is no text field with this id.
    NoTextField(String),
    /// There is no checkbox with this id.
    NoCheckbox(String),
    /// No button has this label.
    NoButton(String),
    /// Several buttons that do different things have this label.
    AmbiguousButton(String),
    /// No suggestions for this text field currently show this line.
    NoSuggestion {
        /// The text field the suggestions are for.
        field: String,
        /// The line that was to be picked.
        line: String,
    },
}

impl fmt::Display for ActionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ActionError::NoTextField(id) => write!(f, "no text field with id {id:?}"),
            ActionError::NoCheckbox(id) => write!(f, "no checkbox with id {id:?}"),
            ActionError::NoButton(label) => write!(f, "no button labelled {label:?}"),
            ActionError::AmbiguousButton(label) => {
                write!(f, "several different buttons are labelled {label:?}")
            }
            ActionError::NoSuggestion { field, line } => {
                write!(
                    f,
                    "no suggestion {line:?} is shown for text field {field:?}"
                )
            }
        }
    }
}

impl std::error::Error for ActionError {}

/// A kettle-ui island without a GUI.
pub struct Island {
    model: Model,
    updates: Updates,
    /// Whether `updates` has ended, so no more updates will come.
    updates_ended: bool,
}

impl Island {
    /// Checks the tree and creates the island (see [`IslandBuilder::build`]).
    /// Computes the initial derived state before returning.
    pub fn new(builder: IslandBuilder) -> Result<Self, Error> {
        let Built {
            model,
            jobs,
            updates,
        } = builder.build()?;
        let mut island = Island {
            model,
            updates,
            updates_ended: false,
        };
        island.settle(jobs);
        Ok(island)
    }

    /// The island's state: field values and named state.
    pub fn model(&self) -> &Model {
        &self.model
    }

    /// The current values of all form elements.
    pub fn fields(&self) -> &Fields {
        self.model.fields()
    }

    /// The current value of named state; `None` if it isn't computed yet.
    pub fn state(&self, name: &str) -> Option<&Value> {
        self.model.state(name)
    }

    /// Replaces the contents of the text field `id`.
    pub fn type_text(&mut self, id: &str, text: &str) -> Result<(), ActionError> {
        let found = find(
            self.model.root(),
            &mut |w| matches!(w.kind(), Kind::TextField { id: i, .. } if &**i == id),
        );
        if found.is_none() {
            return Err(ActionError::NoTextField(id.to_owned()));
        }
        let jobs = self.model.set_text(id, text.to_owned());
        self.settle(jobs);
        Ok(())
    }

    /// Checks or unchecks the checkbox `id`.
    pub fn set_checked(&mut self, id: &str, checked: bool) -> Result<(), ActionError> {
        let found = find(
            self.model.root(),
            &mut |w| matches!(w.kind(), Kind::Checkbox { id: i, .. } if &**i == id),
        );
        if found.is_none() {
            return Err(ActionError::NoCheckbox(id.to_owned()));
        }
        let jobs = self.model.set_checked(id, checked);
        self.settle(jobs);
        Ok(())
    }

    /// Clicks the line `line` of the suggestions for the text field `field`,
    /// which puts it into the field. The line must currently be shown.
    pub fn pick(&mut self, field: &str, line: &str) -> Result<(), ActionError> {
        let model = &self.model;
        let found = find(model.root(), &mut |w| match w.kind() {
            Kind::Suggestions { state, field: f } => {
                &**f == field && list_items(model, state).iter().any(|l| l == line)
            }
            _ => false,
        });
        if found.is_none() {
            return Err(ActionError::NoSuggestion {
                field: field.to_owned(),
                line: line.to_owned(),
            });
        }
        let jobs = self.model.set_text(field, line.to_owned());
        self.settle(jobs);
        Ok(())
    }

    /// Presses the button labelled `label`, and returns how the user finished
    /// with the island. Like in other backends, the island stays usable.
    pub fn press(&mut self, label: &str) -> Result<Outcome, ActionError> {
        let mut actions = Vec::new();
        find(self.model.root(), &mut |w| {
            if let Kind::Button { label: l, action } = w.kind()
                && l == label
            {
                actions.push(*action);
            }
            false
        });
        let action = match actions.as_slice() {
            [] => return Err(ActionError::NoButton(label.to_owned())),
            [first, rest @ ..] => {
                if rest.iter().any(|a| !same_action(*a, *first)) {
                    return Err(ActionError::AmbiguousButton(label.to_owned()));
                }
                *first
            }
        };
        Ok(match action {
            Action::Submit => Outcome::Submitted(self.model.fields().clone()),
            Action::Cancel => Outcome::Cancelled,
        })
    }

    /// Applies the [`StateHandle`](kettle_ui::StateHandle) updates that have
    /// arrived, without waiting for more.
    pub fn poll(&mut self) {
        let waker = Waker::noop();
        self.poll_with(waker);
    }

    /// Applies [`StateHandle`](kettle_ui::StateHandle) updates as they arrive
    /// until `done` returns true, and returns true. Returns false if that
    /// doesn't happen within `timeout`, or if no more updates can come (all
    /// handles are dropped).
    pub fn wait_until(&mut self, timeout: Duration, mut done: impl FnMut(&Self) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        let waker = Waker::from(Arc::new(Unpark(thread::current())));
        loop {
            self.poll_with(&waker);
            if done(self) {
                return true;
            }
            let now = Instant::now();
            if self.updates_ended || now >= deadline {
                return false;
            }
            // Woken when an update arrives; spurious wakeups just loop.
            thread::park_timeout(deadline - now);
        }
    }

    fn poll_with(&mut self, waker: &Waker) {
        let mut cx = Context::from_waker(waker);
        while !self.updates_ended {
            match Pin::new(&mut self.updates).poll_next(&mut cx) {
                Poll::Ready(Some(update)) => {
                    let jobs = self.model.apply(update);
                    self.settle(jobs);
                }
                Poll::Ready(None) => self.updates_ended = true,
                Poll::Pending => break,
            }
        }
    }

    /// Runs the jobs, and the ones they lead to, until there are none left.
    fn settle(&mut self, mut jobs: Vec<Job>) {
        while let Some(job) = jobs.pop() {
            jobs.extend(self.model.finish(job.run()));
        }
    }
}

/// Shows the island as an indented outline: what a user would see, plus the
/// ids needed to act on it. Doesn't apply pending updates; call
/// [`Island::poll`] first for an up-to-date view.
impl fmt::Display for Island {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = String::new();
        render(&self.model, self.model.root(), 0, &mut out)?;
        f.write_str(&out)
    }
}

fn render(model: &Model, widget: &Widget, depth: usize, out: &mut String) -> fmt::Result {
    let indent = "  ".repeat(depth);
    let fields = model.fields();
    let lines = |out: &mut String, state: &str| {
        list_items(model, state)
            .iter()
            .try_for_each(|l| writeln!(out, "{indent}  - {l:?}"))
    };
    match widget.kind() {
        Kind::Column(children) | Kind::Row(children) => {
            let name = match widget.kind() {
                Kind::Column(_) => "column",
                _ => "row",
            };
            writeln!(out, "{indent}{name}")?;
            for child in children {
                render(model, child, depth + 1, out)?;
            }
            Ok(())
        }
        Kind::Label(text) => {
            let text = match text {
                Text::Static(s) => s.clone(),
                Text::State(name) => model.state(name).map(Value::to_string).unwrap_or_default(),
            };
            writeln!(out, "{indent}label {text:?}")
        }
        Kind::TextField { id, placeholder } => {
            let value = fields.text(id);
            write!(out, "{indent}text_field {id}: {value:?}")?;
            if value.is_empty() && !placeholder.is_empty() {
                write!(out, " (placeholder {placeholder:?})")?;
            }
            writeln!(out)
        }
        Kind::Checkbox { id, label } => {
            let mark = if fields.checked(id) { 'x' } else { ' ' };
            writeln!(out, "{indent}checkbox {id}: [{mark}] {label:?}")
        }
        Kind::Progress { state } => {
            let value = match model.state(state) {
                Some(Value::Number(n)) => n.clamp(0.0, 1.0),
                _ => 0.0,
            };
            writeln!(out, "{indent}progress {:.0}%", value * 100.0)
        }
        Kind::List { state } => {
            writeln!(out, "{indent}list")?;
            lines(out, state)
        }
        Kind::Suggestions { state, field } => {
            writeln!(out, "{indent}suggestions for {field}")?;
            lines(out, state)
        }
        Kind::Button { label, .. } => writeln!(out, "{indent}button {label:?}"),
    }
}

/// Named state as a list; empty if it's not a list (or not computed yet).
fn list_items<'a>(model: &'a Model, name: &str) -> &'a [String] {
    match model.state(name) {
        Some(Value::List(items)) => items,
        _ => &[],
    }
}

/// The first widget in the tree that `pred` matches, depth first.
fn find<'a>(widget: &'a Widget, pred: &mut impl FnMut(&Widget) -> bool) -> Option<&'a Widget> {
    if pred(widget) {
        return Some(widget);
    }
    match widget.kind() {
        Kind::Column(children) | Kind::Row(children) => children.iter().find_map(|c| find(c, pred)),
        _ => None,
    }
}

fn same_action(a: Action, b: Action) -> bool {
    matches!(
        (a, b),
        (Action::Submit, Action::Submit) | (Action::Cancel, Action::Cancel)
    )
}

/// Wakes a thread parked in [`Island::wait_until`].
struct Unpark(Thread);

impl Wake for Unpark {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

#[cfg(test)]
mod tests {
    use kettle_ui::widget::*;

    use super::*;

    #[test]
    fn actions_on_missing_widgets_are_errors() {
        let mut island = Island::new(IslandBuilder::new(column([
            text_field("t", ""),
            checkbox("c", ""),
            submit_button("OK"),
            cancel_button("OK"),
        ])))
        .unwrap();
        assert_eq!(
            island.type_text("c", "x"),
            Err(ActionError::NoTextField("c".into()))
        );
        assert_eq!(
            island.set_checked("t", true),
            Err(ActionError::NoCheckbox("t".into()))
        );
        assert_eq!(
            island.pick("t", "x"),
            Err(ActionError::NoSuggestion {
                field: "t".into(),
                line: "x".into()
            })
        );
        assert!(matches!(
            island.press("Nope"),
            Err(ActionError::NoButton(l)) if l == "Nope"
        ));
        assert!(matches!(
            island.press("OK"),
            Err(ActionError::AmbiguousButton(l)) if l == "OK"
        ));
    }

    #[test]
    fn waits_for_state_handle_updates() {
        let mut builder = IslandBuilder::new(column([label(state("s")), list("l")]));
        builder.set_state("s", "start");
        builder.derive_with_state("l", &["s"], |i| {
            vec![i.state("s").unwrap().to_string()].into()
        });
        let handle = builder.handle();
        let mut island = Island::new(builder).unwrap();
        assert_eq!(
            island.to_string(),
            "column\n  label \"start\"\n  list\n    - \"start\"\n"
        );

        let sender = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            handle.set("s", "done");
        });
        let done = |i: &Island| i.state("l") == Some(&Value::List(vec!["done".into()]));
        assert!(island.wait_until(Duration::from_secs(10), done));
        sender.join().unwrap();

        // The handle is gone, so no more updates can come.
        assert!(!island.wait_until(Duration::from_secs(10), |_| false));
    }

    #[test]
    fn wait_times_out() {
        let mut builder = IslandBuilder::new(label(state("s")));
        builder.set_state("s", "");
        let _handle = builder.handle();
        let mut island = Island::new(builder).unwrap();
        assert!(!island.wait_until(Duration::from_millis(10), |_| false));
    }
}
