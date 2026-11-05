//! A deliberately small UI framework.
//!
//! A kettle-ui UI is an *island*: a piece of UI embedded in a host
//! application. The host owns the window, scrolling, and anything else around
//! the island. This crate doesn't depend on any GUI toolkit: it describes the
//! island and keeps its state ([`Model`]); a backend crate such as
//! `kettle-ui-iced` renders it.
//!
//! The model:
//! - The widget tree is built once and never changes.
//! - Widgets keep their own interaction state (cursor, focus, scroll), which is
//!   not accessible from outside.
//! - Form elements (text fields, checkboxes) have values, which the host gets
//!   when the user submits the form.
//! - Labels, progress bars and lists can show *named state*. Named state
//!   either has an initial value ([`IslandBuilder::set_state`]) and can then be
//!   updated from outside through a [`StateHandle`], or is *derived*: computed
//!   from the current field values (e.g. search-as-you-type).
//! - Mistakes in the tree (such as referencing named state that doesn't
//!   exist) are reported by [`IslandBuilder::build`].
//!
//! ```
//! use kettle_ui::IslandBuilder;
//! use kettle_ui::widget::*;
//!
//! let mut builder = IslandBuilder::new(column([
//!     text_field("name", "Your name"),
//!     label(state("greeting")),
//!     submit_button("OK"),
//! ]));
//! builder.derive("greeting", |f| format!("Hello, {}!", f.text("name")).into());
//! // Usually a backend calls this.
//! let built = builder.build().unwrap();
//! ```
//!
//! # Strict mode
//!
//! Some mistakes can only be caught while the UI runs, such as a
//! [`StateHandle`] setting state that has no initial value. In strict mode
//! they panic; otherwise they print a warning to stderr and are ignored.
//! Strict mode is on in debug builds and off in release builds. The
//! `KETTLE_UI_STRICT` environment variable overrides this: `1` turns it on,
//! `0` turns it off.

mod model;
pub mod widget;

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, OnceLock, RwLock};

use futures_channel::mpsc;

pub use model::{Built, Job, JobResult, Model, Outcome, StateUpdate, Updates};

use widget::{Kind, Text, Widget};

/// Why [`IslandBuilder::build`] failed.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// Two form elements share this id.
    DuplicateField(String),
    /// A widget references named state that has no initial value and isn't
    /// derived.
    UnknownState(String),
    /// This named state is derived more than once, or both derived and given
    /// an initial value.
    ConflictingState(String),
    /// A widget references a form element that doesn't exist.
    UnknownField(String),
    /// [`suggestions`](widget::suggestions) targets a form element that isn't a text field.
    NotATextField(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::DuplicateField(id) => write!(f, "duplicate form element id {id:?}"),
            Error::UnknownState(name) => write!(
                f,
                "named state {name:?} is used but has no initial value and isn't derived"
            ),
            Error::ConflictingState(name) => write!(
                f,
                "named state {name:?} is derived more than once, or both derived and set"
            ),
            Error::UnknownField(id) => write!(f, "no form element with id {id:?}"),
            Error::NotATextField(id) => write!(f, "form element {id:?} is not a text field"),
        }
    }
}

impl std::error::Error for Error {}

/// The value of a piece of named state.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// Text, e.g. for a [`label`](widget::label).
    Text(String),
    /// A number, e.g. for a [`progress`](widget::progress) bar (0 to 1).
    Number(f64),
    /// Lines, e.g. for a [`list`](widget::list).
    List(Vec<String>),
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Text(s) => f.write_str(s),
            Value::Number(n) => write!(f, "{n}"),
            Value::List(items) => f.write_str(&items.join(", ")),
        }
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::Text(s.to_owned())
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::Text(s)
    }
}

impl From<f64> for Value {
    fn from(n: f64) -> Self {
        Value::Number(n)
    }
}

impl From<Vec<String>> for Value {
    fn from(items: Vec<String>) -> Self {
        Value::List(items)
    }
}

/// The values of all form elements, by id.
#[derive(Clone, Debug, Default)]
pub struct Fields(HashMap<Arc<str>, Field>);

#[derive(Clone, Debug)]
enum Field {
    Text(String),
    Bool(bool),
}

impl Fields {
    /// The contents of a text field, or `""` if there is no such field.
    pub fn text(&self, id: &str) -> &str {
        match self.0.get(id) {
            Some(Field::Text(s)) => s,
            _ => "",
        }
    }

    /// Whether a checkbox is checked, or `false` if there is no such checkbox.
    pub fn checked(&self, id: &str) -> bool {
        matches!(self.0.get(id), Some(Field::Bool(true)))
    }
}

type DeriveFn = Arc<dyn Fn(&Fields) -> Value + Send + Sync>;

/// Configures an island; create it with [`IslandBuilder::new`], then hand it
/// to a backend (which calls [`IslandBuilder::build`]).
pub struct IslandBuilder {
    root: Widget,
    state: HashMap<String, Value>,
    derived: Vec<(String, DeriveFn)>,
    /// Names that have an initial value, shared with every [`StateHandle`].
    settable: Arc<RwLock<HashSet<String>>>,
    sender: mpsc::UnboundedSender<StateUpdate>,
    receiver: mpsc::UnboundedReceiver<StateUpdate>,
}

impl IslandBuilder {
    /// Starts configuring an island with the given (fixed) widget tree.
    pub fn new(root: Widget) -> Self {
        let (sender, receiver) = mpsc::unbounded();
        IslandBuilder {
            root,
            state: HashMap::new(),
            derived: Vec::new(),
            settable: Arc::default(),
            sender,
            receiver,
        }
    }

    /// Sets the initial value of named state.
    pub fn set_state(&mut self, name: &str, value: impl Into<Value>) {
        self.state.insert(name.to_owned(), value.into());
        self.settable.write().unwrap().insert(name.to_owned());
    }

    /// Makes named state a function of the current field values. It is
    /// recomputed after every change, on a background thread, so it may be
    /// slow (e.g. a search). Results that arrive after a newer change are
    /// dropped.
    pub fn derive(
        &mut self,
        name: &str,
        compute: impl Fn(&Fields) -> Value + Send + Sync + 'static,
    ) {
        self.derived.push((name.to_owned(), Arc::new(compute)));
    }

    /// A handle for setting named state from other threads while the island
    /// is open. Only state with an initial value
    /// ([`IslandBuilder::set_state`]) can be set this way.
    pub fn handle(&self) -> StateHandle {
        StateHandle {
            settable: self.settable.clone(),
            sender: self.sender.clone(),
        }
    }

    /// Checks the tree and creates the island's [`Model`]. For the island to
    /// work, the caller must run the returned jobs and deliver the updates
    /// (see [`Built`]).
    pub fn build(self) -> Result<Built, Error> {
        let fields = self.check()?;
        let (model, jobs) = Model::new(self.root, fields, self.state, self.derived);
        Ok(Built {
            model,
            jobs,
            updates: Updates(self.receiver),
        })
    }

    /// Checks that everything the tree references exists. Returns the initial
    /// (empty) field values.
    fn check(&self) -> Result<Fields, Error> {
        for (i, (name, _)) in self.derived.iter().enumerate() {
            let derived_earlier = self.derived[..i].iter().any(|(n, _)| n == name);
            if derived_earlier || self.state.contains_key(name) {
                return Err(Error::ConflictingState(name.clone()));
            }
        }
        let mut fields = Fields::default();
        collect_fields(&self.root, &mut fields)?;
        self.check_references(&self.root, &fields)?;
        Ok(fields)
    }

    fn check_references(&self, widget: &Widget, fields: &Fields) -> Result<(), Error> {
        let state = match widget.kind() {
            Kind::Column(children) | Kind::Row(children) => {
                return children
                    .iter()
                    .try_for_each(|c| self.check_references(c, fields));
            }
            Kind::Label(Text::State(state)) | Kind::Progress { state } | Kind::List { state } => {
                state
            }
            Kind::Suggestions { state, field } => {
                match fields.0.get(field) {
                    None => return Err(Error::UnknownField(field.to_string())),
                    Some(Field::Bool(_)) => return Err(Error::NotATextField(field.to_string())),
                    Some(Field::Text(_)) => {}
                }
                state
            }
            _ => return Ok(()),
        };
        if self.state.contains_key(state) || self.derived.iter().any(|(n, _)| n == state) {
            Ok(())
        } else {
            Err(Error::UnknownState(state.clone()))
        }
    }
}

fn collect_fields(widget: &Widget, fields: &mut Fields) -> Result<(), Error> {
    let (id, value) = match widget.kind() {
        Kind::Column(children) | Kind::Row(children) => {
            return children.iter().try_for_each(|c| collect_fields(c, fields));
        }
        Kind::TextField { id, .. } => (id, Field::Text(String::new())),
        Kind::Checkbox { id, .. } => (id, Field::Bool(false)),
        _ => return Ok(()),
    };
    match fields.0.insert(id.clone(), value) {
        Some(_) => Err(Error::DuplicateField(id.to_string())),
        None => Ok(()),
    }
}

/// Sets named state of an island. Cheap to clone and usable from any thread.
/// Updates sent after the island is dropped are ignored.
#[derive(Clone)]
pub struct StateHandle {
    settable: Arc<RwLock<HashSet<String>>>,
    sender: mpsc::UnboundedSender<StateUpdate>,
}

impl StateHandle {
    /// Sets named state. The name must already have an initial value
    /// ([`IslandBuilder::set_state`]); otherwise this panics in [strict
    /// mode](crate#strict-mode), or prints a warning and does nothing.
    #[track_caller]
    pub fn set(&self, name: &str, value: impl Into<Value>) {
        self.set_with(strict(), name, value.into());
    }

    #[track_caller]
    fn set_with(&self, strict: bool, name: &str, value: Value) {
        if !self.settable.read().unwrap().contains(name) {
            misuse(
                strict,
                format!(
                    "named state {name:?} has no initial value, so it can't be set \
                     (give it one with IslandBuilder::set_state)"
                ),
                "ignoring the update",
            );
            return;
        }
        let name = name.to_owned();
        let _ = self.sender.unbounded_send(StateUpdate { name, value });
    }
}

/// Reports a mistake that is only caught at run time: panics in [strict
/// mode](crate#strict-mode), otherwise prints a warning saying what happens
/// instead (`fallback`).
#[track_caller]
fn misuse(strict: bool, message: String, fallback: &str) {
    if strict {
        panic!("{message}");
    }
    eprintln!("kettle-ui: warning: {message}; {fallback}");
}

/// Whether [strict mode](crate#strict-mode) is on.
fn strict() -> bool {
    static STRICT: OnceLock<bool> = OnceLock::new();
    *STRICT.get_or_init(|| match std::env::var("KETTLE_UI_STRICT").as_deref() {
        Ok("1") => true,
        Ok("0") => false,
        Ok(other) => {
            eprintln!("kettle-ui: warning: ignoring KETTLE_UI_STRICT={other:?} (expected 0 or 1)");
            cfg!(debug_assertions)
        }
        Err(_) => cfg!(debug_assertions),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::*;

    fn check(ui: &IslandBuilder) -> Result<(), Error> {
        ui.check().map(|_| ())
    }

    #[test]
    fn valid_tree_passes() {
        let mut ui = IslandBuilder::new(column([
            text_field("q", ""),
            label(state("status")),
            progress("progress"),
            suggestions("matches", "q"),
        ]));
        ui.set_state("status", "");
        ui.set_state("progress", 0.0);
        ui.derive("matches", |_| Vec::new().into());
        assert!(check(&ui).is_ok());
    }

    #[test]
    fn missing_state_is_an_error() {
        let ui = IslandBuilder::new(column([label(state("status"))]));
        assert!(matches!(check(&ui), Err(Error::UnknownState(n)) if n == "status"));
    }

    #[test]
    fn duplicate_field_is_an_error() {
        let ui = IslandBuilder::new(column([text_field("a", ""), checkbox("a", "")]));
        assert!(matches!(check(&ui), Err(Error::DuplicateField(n)) if n == "a"));
    }

    #[test]
    fn state_both_set_and_derived_is_an_error() {
        let mut ui = IslandBuilder::new(label(state("s")));
        ui.set_state("s", "");
        ui.derive("s", |_| "".into());
        assert!(matches!(check(&ui), Err(Error::ConflictingState(n)) if n == "s"));
    }

    #[test]
    fn setting_state_with_initial_value_works() {
        let mut ui = IslandBuilder::new(label(state("s")));
        ui.set_state("s", "a");
        ui.handle().set_with(true, "s", "b".into());
        let update = ui.receiver.try_recv().unwrap();
        assert_eq!(
            update,
            StateUpdate {
                name: "s".into(),
                value: "b".into()
            }
        );
    }

    #[test]
    #[should_panic(expected = "has no initial value")]
    fn setting_unknown_state_panics_in_strict_mode() {
        let ui = IslandBuilder::new(column([]));
        ui.handle().set_with(true, "s", "b".into());
    }

    #[test]
    fn setting_unknown_state_is_ignored_otherwise() {
        let mut ui = IslandBuilder::new(column([]));
        ui.handle().set_with(false, "s", "b".into());
        assert!(ui.receiver.try_recv().is_err());
    }

    #[test]
    fn suggestions_target_must_be_a_text_field() {
        let mut ui = IslandBuilder::new(column([checkbox("c", ""), suggestions("m", "c")]));
        ui.set_state("m", Vec::new());
        assert!(matches!(check(&ui), Err(Error::NotATextField(n)) if n == "c"));

        let mut ui = IslandBuilder::new(suggestions("m", "nope"));
        ui.set_state("m", Vec::new());
        assert!(matches!(check(&ui), Err(Error::UnknownField(n)) if n == "nope"));
    }
}
