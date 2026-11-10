//! A headless backend for kettle-ui, for tests and for driving islands
//! programmatically (e.g. by an agent).
//!
//! An [`Island`] acts like a user would: it types into text fields, toggles
//! checkboxes, picks suggestions and presses buttons, all by the ids and
//! labels a user would see. It shows the island as text ([`Island`]
//! implements [`Display`](fmt::Display)); [`outline`] does the same for a
//! [`FrozenIsland`].
//!
//! Derived state is computed on the calling thread as part of each action, so
//! after an action returns, everything derived from the fields is up to date.
//! Updates from [`StateHandle`](kettle_ui::StateHandle)s arrive from other
//! threads: apply them with [`Island::poll`], or wait for them with
//! [`Island::wait_until`]. An event loop can use [`Island::poll_waking`] to be
//! told when more arrive.
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
//!     "column\n  text_field name: \"Ada\"\n  label \"Hello, Ada!\"\n  button \"OK\" (submit)\n",
//! );
//! ```

use std::fmt::{self, Write};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};
use std::time::{Duration, Instant};

use futures_core::Stream;
use kettle_ui::widget::{Action, Borders, Kind, Text, Widget, is_shown};
use kettle_ui::{
    Built, Contents, Error, Fields, FrozenIsland, IslandBuilder, Job, Model, Outcome, Updates,
    Value,
};

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
    /// A submit button was pressed, but validators found these errors (see
    /// [`IslandBuilder::validate`]). The island now shows them.
    Invalid(Vec<String>),
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
            ActionError::Invalid(errors) => write!(f, "not submitted: {}", errors.join("; ")),
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

    /// A snapshot of the island that can't change (see [`Model::freeze`]).
    pub fn freeze(&self) -> FrozenIsland {
        self.model.freeze()
    }

    /// The current values of all form elements.
    pub fn fields(&self) -> &Fields {
        self.model.fields()
    }

    /// The current value of named state; `None` if it isn't computed yet.
    pub fn state(&self, name: &str) -> Option<&Value> {
        self.model.state(name)
    }

    /// Replaces the contents of the text field or text area `id`.
    pub fn type_text(&mut self, id: &str, text: &str) -> Result<(), ActionError> {
        let found = find(&self.model, self.model.root(), &mut |w| match w.kind() {
            Kind::TextField { id: i, .. } | Kind::TextArea { id: i, .. } => &**i == id,
            _ => false,
        });
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
            &self.model,
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
        let found = find(model, model.root(), &mut |w| match w.kind() {
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
    /// A submit button submits only if the validators pass; otherwise this
    /// returns [`ActionError::Invalid`].
    pub fn press(&mut self, label: &str) -> Result<Outcome, ActionError> {
        let mut actions = Vec::new();
        find(&self.model, self.model.root(), &mut |w| {
            if let Kind::Button {
                label: l, action, ..
            } = w.kind()
                && l == label
            {
                actions.push(action.clone());
            }
            false
        });
        let action = match actions.as_slice() {
            [] => return Err(ActionError::NoButton(label.to_owned())),
            [first, rest @ ..] => {
                if rest.iter().any(|a| a != first) {
                    return Err(ActionError::AmbiguousButton(label.to_owned()));
                }
                first.clone()
            }
        };
        Ok(match action {
            Action::Submit => {
                let jobs = self.model.submit();
                self.settle(jobs);
                // Settled, so the validators are up to date.
                match self.model.take_submitted() {
                    Some(fields) => Outcome::Submitted(fields),
                    None => {
                        let errors = self.model.validation_errors();
                        let errors = errors.into_iter().map(str::to_owned).collect();
                        return Err(ActionError::Invalid(errors));
                    }
                }
            }
            Action::Cancel => Outcome::Cancelled,
            Action::Press(id) => Outcome::Pressed(id, self.model.fields().clone()),
        })
    }

    /// Applies the [`StateHandle`](kettle_ui::StateHandle) updates that have
    /// arrived, without waiting for more. Returns whether there were any.
    pub fn poll(&mut self) -> bool {
        self.poll_waking(Waker::noop())
    }

    /// Like [`poll`](Self::poll), but `waker` is woken when the next update
    /// arrives (then call this again).
    pub fn poll_waking(&mut self, waker: &Waker) -> bool {
        self.poll_with(waker)
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

    /// Applies the updates that have arrived; returns whether there were any.
    fn poll_with(&mut self, waker: &Waker) -> bool {
        let mut cx = Context::from_waker(waker);
        let mut any = false;
        while !self.updates_ended {
            match Pin::new(&mut self.updates).poll_next(&mut cx) {
                Poll::Ready(Some(update)) => {
                    let jobs = self.model.apply(update);
                    self.settle(jobs);
                    any = true;
                }
                Poll::Ready(None) => self.updates_ended = true,
                Poll::Pending => break,
            }
        }
        any
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
        f.write_str(&outline(&self.model))
    }
}

/// Shows an island as an indented outline, like [`Island`]'s
/// [`Display`](fmt::Display) does. Use it for a [`FrozenIsland`].
pub fn outline(contents: &impl Contents) -> String {
    let mut out = String::new();
    render(contents, contents.root(), 0, &mut out).expect("writing to a String can't fail");
    out
}

fn render(model: &impl Contents, widget: &Widget, depth: usize, out: &mut String) -> fmt::Result {
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
        Kind::Table { rows, borders } => {
            match borders {
                Borders::All => writeln!(out, "{indent}table")?,
                Borders::Horizontal => writeln!(out, "{indent}table (horizontal borders)")?,
            }
            for cells in rows {
                writeln!(out, "{indent}  row")?;
                for cell in cells {
                    render(model, cell, depth + 2, out)?;
                }
            }
            Ok(())
        }
        // A shown child appears as if it weren't wrapped; a hidden one not at all.
        Kind::When { state, child } if is_shown(model, state) => render(model, child, depth, out),
        Kind::When { .. } => Ok(()),
        Kind::Label(text) => {
            let text = match text {
                Text::Static(s) => s.clone(),
                Text::State(name) => model.state(name).map(Value::to_string).unwrap_or_default(),
            };
            writeln!(out, "{indent}label {text:?}")
        }
        Kind::TextField { id, placeholder } | Kind::TextArea { id, placeholder } => {
            let name = match widget.kind() {
                Kind::TextField { .. } => "text_field",
                _ => "text_area",
            };
            let value = fields.text(id);
            // Debug formatting keeps a multi-line value on one line.
            write!(out, "{indent}{name} {id}: {value:?}")?;
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
        Kind::Image { state } => match model.state(state) {
            Some(Value::Image(image)) => {
                write!(out, "{indent}image {:?} (", image.alt())?;
                match image.data() {
                    Some(data) => write!(out, "{}x{}", data.width(), data.height())?,
                    None => write!(out, "not available")?,
                }
                if let Some(path) = image.path() {
                    write!(out, ", {}", path.display())?;
                }
                writeln!(out, ")")
            }
            _ => writeln!(out, "{indent}image (none)"),
        },
        Kind::List { state } => {
            writeln!(out, "{indent}list")?;
            lines(out, state)
        }
        Kind::Suggestions { state, field } => {
            writeln!(out, "{indent}suggestions for {field}")?;
            lines(out, state)
        }
        Kind::Button {
            label,
            key,
            action,
            danger,
        } => {
            write!(out, "{indent}button {label:?}")?;
            if let Some(key) = key {
                write!(out, " [{key}]")?;
            }
            match action {
                Action::Submit => write!(out, " (submit)")?,
                Action::Cancel => write!(out, " (cancel)")?,
                Action::Press(_) => {}
            }
            if *danger {
                write!(out, " (danger)")?;
            }
            writeln!(out)
        }
    }
}

/// Named state as a list; empty if it's not a list (or not computed yet).
fn list_items<'a>(model: &'a impl Contents, name: &str) -> &'a [String] {
    match model.state(name) {
        Some(Value::List(items)) => items,
        _ => &[],
    }
}

/// The first widget in the tree that `pred` matches, depth first. Skips
/// widgets hidden by [`when`](kettle_ui::widget::when), since a user can't
/// use them.
fn find<'a>(
    model: &impl Contents,
    widget: &'a Widget,
    pred: &mut impl FnMut(&Widget) -> bool,
) -> Option<&'a Widget> {
    if pred(widget) {
        return Some(widget);
    }
    match widget.kind() {
        Kind::Column(children) | Kind::Row(children) => {
            children.iter().find_map(|c| find(model, c, pred))
        }
        Kind::Table { rows, .. } => rows.iter().flatten().find_map(|c| find(model, c, pred)),
        Kind::When { state, child } if is_shown(model, state) => find(model, child, pred),
        _ => None,
    }
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
    fn text_areas_take_typed_text() {
        let mut island = Island::new(IslandBuilder::new(text_area("a", "Notes"))).unwrap();
        assert_eq!(
            island.to_string(),
            "text_area a: \"\" (placeholder \"Notes\")\n"
        );
        island.type_text("a", "one\ntwo").unwrap();
        assert_eq!(island.fields().text("a"), "one\ntwo");
        assert_eq!(island.to_string(), "text_area a: \"one\\ntwo\"\n");
    }

    #[test]
    fn tables_hold_fields() {
        let mut island = Island::new(IslandBuilder::new(table_horizontal([
            vec![label("Name"), text_field("name", "")],
            vec![checkbox("ok", "OK")],
        ])))
        .unwrap();
        island.type_text("name", "Ada").unwrap();
        island.set_checked("ok", true).unwrap();
        assert_eq!(
            island.to_string(),
            "table (horizontal borders)\n  row\n    label \"Name\"\n    \
             text_field name: \"Ada\"\n  row\n    checkbox ok: [x] \"OK\"\n"
        );
    }

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
    fn button_keys_are_in_the_outline() {
        let island = Island::new(IslandBuilder::new(row([
            submit_button("Yes [y]"),
            cancel_button("No"),
        ])))
        .unwrap();
        assert_eq!(
            island.to_string(),
            "row\n  button \"Yes\" [y] (submit)\n  button \"No\" (cancel)\n"
        );
    }

    #[test]
    fn images_are_in_the_outline() {
        let red = kettle_ui::Image::from_rgba(2, 1, &[255, 0, 0, 255].repeat(2), "Red").unwrap();
        let mut b = IslandBuilder::new(column([image("a"), image("b"), image("c")]));
        b.set_state("a", red.clone());
        b.set_state("b", red.without_data());
        b.set_state("c", "text");
        let island = b.freeze().unwrap();
        assert_eq!(
            outline(&island),
            "column\n  image \"Red\" (2x1)\n  image \"Red\" (not available)\n  image (none)\n"
        );
    }

    #[test]
    fn danger_buttons_are_marked_in_the_outline() {
        let island = Island::new(IslandBuilder::new(button_danger("del", "Delete [d]"))).unwrap();
        assert_eq!(island.to_string(), "button \"Delete\" [d] (danger)\n");
    }

    #[test]
    fn pressing_a_button_reports_its_id_and_fields() {
        let mut island = Island::new(IslandBuilder::new(column([
            text_field("t", ""),
            button("del", "Delete"),
        ])))
        .unwrap();
        island.type_text("t", "x").unwrap();
        let Ok(Outcome::Pressed(id, fields)) = island.press("Delete") else {
            panic!("not pressed");
        };
        assert_eq!(&*id, "del");
        assert_eq!(fields.text("t"), "x");
    }

    #[test]
    fn only_submit_buttons_validate() {
        let mut builder = IslandBuilder::new(column([
            text_field("t", ""),
            label(state("error")),
            row([submit_button("OK"), button("other", "Other")]),
        ]));
        builder.validate("error", |i| i.text("t").is_empty().then(|| "Empty".into()));
        let mut island = Island::new(builder).unwrap();
        assert!(island.to_string().contains("label \"\"\n"));
        assert!(matches!(island.press("Other"), Ok(Outcome::Pressed(..))));

        let error = island.press("OK").unwrap_err();
        assert_eq!(error, ActionError::Invalid(vec!["Empty".into()]));
        assert_eq!(error.to_string(), "not submitted: Empty");
        assert!(island.to_string().contains("label \"Empty\"\n"));

        island.type_text("t", "x").unwrap();
        assert!(island.to_string().contains("label \"\"\n"));
        let Ok(Outcome::Submitted(fields)) = island.press("OK") else {
            panic!("not submitted");
        };
        assert_eq!(fields.text("t"), "x");
    }

    #[test]
    fn hidden_widgets_are_left_out() {
        let mut builder = IslandBuilder::new(column([
            checkbox("more", "More"),
            when("more", column([text_field("t", ""), submit_button("OK")])),
        ]));
        builder.derive("more", |f| f.checked("more").into());
        let mut island = Island::new(builder).unwrap();
        assert_eq!(
            island.to_string(),
            "column\n  checkbox more: [ ] \"More\"\n"
        );
        assert_eq!(
            island.type_text("t", "x"),
            Err(ActionError::NoTextField("t".into()))
        );
        assert!(matches!(island.press("OK"), Err(ActionError::NoButton(_))));

        island.set_checked("more", true).unwrap();
        assert_eq!(
            island.to_string(),
            "column\n  checkbox more: [x] \"More\"\n  column\n    text_field t: \"\"\n    button \"OK\" (submit)\n"
        );
        island.type_text("t", "x").unwrap();
        assert!(island.press("OK").is_ok());
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
    fn frozen_island_has_the_same_outline() {
        let mut island = Island::new(IslandBuilder::new(column([
            text_field("t", ""),
            checkbox("c", "C"),
        ])))
        .unwrap();
        island.type_text("t", "x").unwrap();
        let frozen = island.freeze();
        assert_eq!(outline(&frozen), island.to_string());
        island.set_checked("c", true).unwrap();
        assert_ne!(outline(&frozen), island.to_string());
    }

    #[test]
    fn poll_waking_wakes_when_an_update_arrives() {
        struct Flag(std::sync::atomic::AtomicBool);
        impl Wake for Flag {
            fn wake(self: Arc<Self>) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let woken = |flag: &Flag| flag.0.load(std::sync::atomic::Ordering::SeqCst);

        let mut builder = IslandBuilder::new(label(state("s")));
        builder.set_state("s", "start");
        let handle = builder.handle();
        let mut island = Island::new(builder).unwrap();
        let flag = Arc::new(Flag(false.into()));
        let waker = Waker::from(flag.clone());
        assert!(!island.poll_waking(&waker));
        assert!(!woken(&flag));
        handle.set("s", "done");
        assert!(woken(&flag));
        assert!(island.poll_waking(&waker));
        assert_eq!(island.state("s"), Some(&"done".into()));
        assert!(!island.poll());
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
