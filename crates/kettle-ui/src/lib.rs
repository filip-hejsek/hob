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
//!   from the current field values (e.g. search-as-you-type), and possibly
//!   from other named state ([`IslandBuilder::derive_with_state`]).
//! - Mistakes in the tree (such as referencing named state that doesn't
//!   exist) are reported by [`IslandBuilder::build`].
//! - A [`FrozenIsland`] can't change: its fields and state are fixed. It's
//!   either created frozen ([`IslandBuilder::freeze`]) or a snapshot of a live
//!   island ([`Model::freeze`]). It can be serialized with serde, e.g. to save
//!   it.
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
//! they panic; otherwise they're logged as warnings (with the `log` crate)
//! and ignored. Without a logger (`log::max_level()` is still `Off`), as in
//! tests, warnings go to stderr instead, so they aren't lost.
//! Strict mode is on in debug builds and off in release builds. The
//! `KETTLE_UI_STRICT` environment variable overrides this: `1` turns it on,
//! `0` turns it off.

mod frozen;
mod model;
pub mod widget;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, OnceLock, RwLock};

use futures_channel::mpsc;
use serde::{Deserialize, Serialize};

pub use frozen::FrozenIsland;
pub use model::{Built, Inputs, Job, JobResult, Model, Outcome, StateUpdate, Updates};

use widget::{Kind, Text, Widget};

/// Why [`IslandBuilder::build`] failed.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// Two form elements share this id.
    DuplicateField(String),
    /// A widget or derived state references named state that has no initial
    /// value and isn't derived.
    UnknownState(String),
    /// This derived state depends on itself, directly or through other
    /// derived state.
    DependencyCycle(String),
    /// This named state is derived more than once, or both derived and given
    /// an initial value.
    ConflictingState(String),
    /// A widget references a form element that doesn't exist.
    UnknownField(String),
    /// [`suggestions`](widget::suggestions) or [`IslandBuilder::set_text`]
    /// targets a form element that isn't a text field.
    NotATextField(String),
    /// [`IslandBuilder::set_checked`] targets a form element that isn't a
    /// checkbox.
    NotACheckbox(String),
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
            Error::DependencyCycle(name) => {
                write!(f, "derived state {name:?} depends on itself")
            }
            Error::UnknownField(id) => write!(f, "no form element with id {id:?}"),
            Error::NotATextField(id) => write!(f, "form element {id:?} is not a text field"),
            Error::NotACheckbox(id) => write!(f, "form element {id:?} is not a checkbox"),
        }
    }
}

impl std::error::Error for Error {}

/// The value of a piece of named state. Serialized as a plain string,
/// number, list or bool. JSON has no NaN or infinity: they're saved as
/// `null`, and `null` is restored as NaN.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Value {
    /// Text, e.g. for a [`label`](widget::label).
    Text(String),
    /// A number, e.g. for a [`progress`](widget::progress) bar (0 to 1).
    Number(f64),
    /// Lines, e.g. for a [`list`](widget::list).
    List(Vec<String>),
    /// A flag, e.g. for showing or hiding widgets (planned).
    Bool(bool),
}

impl<'de> Deserialize<'de> for Value {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Text(String),
            /// `None` for `null`, which is how non-finite numbers are saved.
            Number(Option<f64>),
            List(Vec<String>),
            Bool(bool),
        }
        Ok(match Repr::deserialize(deserializer)? {
            Repr::Text(s) => Value::Text(s),
            Repr::Number(n) => Value::Number(n.unwrap_or(f64::NAN)),
            Repr::List(items) => Value::List(items),
            Repr::Bool(b) => Value::Bool(b),
        })
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Text(s) => f.write_str(s),
            Value::Number(n) => write!(f, "{n}"),
            Value::List(items) => f.write_str(&items.join(", ")),
            Value::Bool(b) => write!(f, "{b}"),
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

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

/// The values of all form elements, by id.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Fields(BTreeMap<Arc<str>, Field>);

/// Serialized as a plain string or bool.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
enum Field {
    Text(String),
    Bool(bool),
}

impl Fields {
    /// Sets the value of an existing field of the same kind.
    fn set(&mut self, id: &str, value: Field) -> Result<(), Error> {
        match (self.0.get_mut(id), &value) {
            (None, _) => Err(Error::UnknownField(id.to_owned())),
            (Some(field @ Field::Text(_)), Field::Text(_))
            | (Some(field @ Field::Bool(_)), Field::Bool(_)) => {
                *field = value;
                Ok(())
            }
            (Some(_), Field::Text(_)) => Err(Error::NotATextField(id.to_owned())),
            (Some(_), Field::Bool(_)) => Err(Error::NotACheckbox(id.to_owned())),
        }
    }

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

/// What an island shows: its widget tree, field values and named state.
/// Implemented by live ([`Model`]) and frozen ([`FrozenIsland`]) islands, so
/// backends can show both the same way.
pub trait Contents {
    /// The widget tree.
    fn root(&self) -> &Widget;
    /// The values of all form elements.
    fn fields(&self) -> &Fields;
    /// The value of named state; `None` if it isn't computed.
    fn state(&self, name: &str) -> Option<&Value>;
    /// Whether named state may be outdated (see [`Model::is_outdated`]).
    fn is_outdated(&self, name: &str) -> bool;
}

type DeriveFn = Arc<dyn Fn(&Inputs<'_>) -> Value + Send + Sync>;

/// Derived state, as configured on the builder.
struct DeriveSpec {
    name: String,
    /// The named state it may read.
    deps: Vec<String>,
    compute: DeriveFn,
}

/// Configures an island; create it with [`IslandBuilder::new`], then hand it
/// to a backend (which calls [`IslandBuilder::build`]).
pub struct IslandBuilder {
    root: Widget,
    state: HashMap<String, Value>,
    /// Initial field values, by id.
    initial: Vec<(String, Field)>,
    derived: Vec<DeriveSpec>,
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
            initial: Vec::new(),
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

    /// Sets the initial contents of a text field (empty by default).
    pub fn set_text(&mut self, id: &str, value: &str) {
        self.initial
            .push((id.to_owned(), Field::Text(value.to_owned())));
    }

    /// Sets whether a checkbox is initially checked (unchecked by default).
    pub fn set_checked(&mut self, id: &str, checked: bool) {
        self.initial.push((id.to_owned(), Field::Bool(checked)));
    }

    /// Makes named state a function of the current field values.
    ///
    /// It's computed on a background thread, so it may be slow (e.g. a
    /// search). The fields it reads are recorded, and it's recomputed only
    /// when one of them changes. At most one computation runs at a time; if a
    /// field it read changes meanwhile, it's rerun once it finishes (and the
    /// outdated result is shown until then).
    pub fn derive(
        &mut self,
        name: &str,
        compute: impl Fn(&Inputs<'_>) -> Value + Send + Sync + 'static,
    ) {
        self.derive_with_state(name, &[], compute);
    }

    /// Like [`derive`](IslandBuilder::derive), but the computation may also
    /// read the named state in `state` (and no other). It's recomputed when
    /// that changes too. If some of it is derived, it's computed first, and
    /// this waits until it's up to date.
    pub fn derive_with_state(
        &mut self,
        name: &str,
        state: &[&str],
        compute: impl Fn(&Inputs<'_>) -> Value + Send + Sync + 'static,
    ) {
        self.derived.push(DeriveSpec {
            name: name.to_owned(),
            deps: state.iter().map(|&s| s.to_owned()).collect(),
            compute: Arc::new(compute),
        });
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
        let (fields, order) = self.check()?;
        let mut derived: Vec<_> = self.derived.into_iter().map(Some).collect();
        let derived = order.iter().map(|&i| derived[i].take().unwrap()).collect();
        let (model, jobs) = Model::new(self.root, fields, self.state, derived);
        Ok(Built {
            model,
            jobs,
            updates: Updates(self.receiver),
        })
    }

    /// Builds the island, computes its derived state on this thread, and
    /// freezes it (see [`Model::freeze`]). For showing an island that can't
    /// change, e.g. the result of a command.
    pub fn freeze(self) -> Result<FrozenIsland, Error> {
        let Built {
            mut model,
            mut jobs,
            ..
        } = self.build()?;
        while let Some(job) = jobs.pop() {
            jobs.extend(model.finish(job.run()));
        }
        Ok(model.freeze())
    }

    /// Checks that everything the tree and derived state reference exists.
    /// Returns the initial field values, and the order in which to
    /// compute derived state (as indices into `self.derived`).
    fn check(&self) -> Result<(Fields, Vec<usize>), Error> {
        for (i, spec) in self.derived.iter().enumerate() {
            let derived_earlier = self.derived[..i].iter().any(|d| d.name == spec.name);
            if derived_earlier || self.state.contains_key(&spec.name) {
                return Err(Error::ConflictingState(spec.name.clone()));
            }
            if let Some(dep) = spec.deps.iter().find(|d| !self.has_state(d)) {
                return Err(Error::UnknownState(dep.clone()));
            }
        }
        let order = self.dependency_order()?;
        let mut fields = Fields::default();
        collect_fields(&self.root, &mut fields)?;
        self.check_references(&self.root, &fields)?;
        for (id, value) in &self.initial {
            fields.set(id, value.clone())?;
        }
        Ok((fields, order))
    }

    fn has_state(&self, name: &str) -> bool {
        self.state.contains_key(name) || self.derived.iter().any(|d| d.name == name)
    }

    /// Orders derived state so that each comes after the derived state it
    /// depends on.
    fn dependency_order(&self) -> Result<Vec<usize>, Error> {
        #[derive(Clone, Copy, PartialEq)]
        enum Mark {
            New,
            Visiting,
            Done,
        }

        fn visit(
            i: usize,
            derived: &[DeriveSpec],
            marks: &mut [Mark],
            order: &mut Vec<usize>,
        ) -> Result<(), Error> {
            match marks[i] {
                Mark::Done => return Ok(()),
                Mark::Visiting => return Err(Error::DependencyCycle(derived[i].name.clone())),
                Mark::New => {}
            }
            marks[i] = Mark::Visiting;
            for dep in &derived[i].deps {
                if let Some(j) = derived.iter().position(|d| &d.name == dep) {
                    visit(j, derived, marks, order)?;
                }
            }
            marks[i] = Mark::Done;
            order.push(i);
            Ok(())
        }

        let mut marks = vec![Mark::New; self.derived.len()];
        let mut order = Vec::with_capacity(self.derived.len());
        for i in 0..self.derived.len() {
            visit(i, &self.derived, &mut marks, &mut order)?;
        }
        Ok(order)
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
        if self.has_state(state) {
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
/// mode](crate#strict-mode), otherwise logs a warning saying what happens
/// instead (`fallback`).
#[track_caller]
fn misuse(strict: bool, message: String, fallback: &str) {
    if strict {
        panic!("{message}");
    }
    warn(format_args!("{message}; {fallback}"));
}

/// Logs a warning, or prints it to stderr if no logger is installed. (Test
/// harnesses capture stderr and show it for failing tests.)
fn warn(message: fmt::Arguments<'_>) {
    if log::max_level() == log::LevelFilter::Off {
        eprintln!("kettle-ui: warning: {message}");
    } else {
        log::warn!("{message}");
    }
}

/// Whether [strict mode](crate#strict-mode) is on.
fn strict() -> bool {
    static STRICT: OnceLock<bool> = OnceLock::new();
    *STRICT.get_or_init(|| match std::env::var("KETTLE_UI_STRICT").as_deref() {
        Ok("1") => true,
        Ok("0") => false,
        Ok(other) => {
            warn(format_args!(
                "ignoring KETTLE_UI_STRICT={other:?} (expected 0 or 1)"
            ));
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
        let warnings = capture_warnings(|| ui.handle().set_with(false, "s", "b".into()));
        assert!(ui.receiver.try_recv().is_err());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("ignoring the update"), "{warnings:?}");
    }

    thread_local! {
        static WARNINGS: std::cell::RefCell<Vec<String>> = const {
            std::cell::RefCell::new(Vec::new())
        };
    }

    /// Collects warnings logged on the current thread.
    struct Capture;

    impl log::Log for Capture {
        fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
            metadata.level() <= log::Level::Warn
        }

        fn log(&self, record: &log::Record<'_>) {
            if self.enabled(record.metadata()) {
                WARNINGS.with(|w| w.borrow_mut().push(record.args().to_string()));
            }
        }

        fn flush(&self) {}
    }

    /// Runs `f`, and returns the warnings it logged on this thread.
    fn capture_warnings(f: impl FnOnce()) -> Vec<String> {
        static INSTALL: std::sync::Once = std::sync::Once::new();
        INSTALL.call_once(|| {
            log::set_logger(&Capture).unwrap();
            log::set_max_level(log::LevelFilter::Warn);
        });
        WARNINGS.with(|w| w.borrow_mut().clear());
        f();
        WARNINGS.with(|w| w.take())
    }

    #[test]
    fn unknown_dependency_is_an_error() {
        let mut ui = IslandBuilder::new(column([]));
        ui.derive_with_state("x", &["nope"], |_| "".into());
        assert!(matches!(check(&ui), Err(Error::UnknownState(n)) if n == "nope"));
    }

    #[test]
    fn dependency_cycle_is_an_error() {
        let mut ui = IslandBuilder::new(column([]));
        ui.derive_with_state("x", &["y"], |_| "".into());
        ui.derive_with_state("y", &["x"], |_| "".into());
        assert!(matches!(check(&ui), Err(Error::DependencyCycle(_))));

        let mut ui = IslandBuilder::new(column([]));
        ui.derive_with_state("x", &["x"], |_| "".into());
        assert!(matches!(check(&ui), Err(Error::DependencyCycle(n)) if n == "x"));
    }

    #[test]
    fn derived_state_is_ordered_by_dependencies() {
        let mut ui = IslandBuilder::new(column([]));
        ui.derive_with_state("c", &["b"], |_| "".into());
        ui.derive_with_state("b", &["a"], |_| "".into());
        ui.derive("a", |_| "".into());
        assert_eq!(ui.check().unwrap().1, [2, 1, 0]);
    }

    #[test]
    fn initial_field_values_are_checked() {
        let mut ui = IslandBuilder::new(column([text_field("t", ""), checkbox("c", "")]));
        ui.set_text("t", "x");
        ui.set_checked("c", true);
        let (fields, _) = ui.check().unwrap();
        assert_eq!(fields.text("t"), "x");
        assert!(fields.checked("c"));

        let mut ui = IslandBuilder::new(checkbox("c", ""));
        ui.set_text("c", "x");
        assert!(matches!(check(&ui), Err(Error::NotATextField(n)) if n == "c"));

        let mut ui = IslandBuilder::new(text_field("t", ""));
        ui.set_checked("t", true);
        assert!(matches!(check(&ui), Err(Error::NotACheckbox(n)) if n == "t"));

        let mut ui = IslandBuilder::new(column([]));
        ui.set_text("nope", "x");
        assert!(matches!(check(&ui), Err(Error::UnknownField(n)) if n == "nope"));
    }

    #[test]
    fn freezing_a_builder_computes_derived_state() {
        let mut ui = IslandBuilder::new(column([text_field("name", ""), label(state("hi"))]));
        ui.set_text("name", "Ada");
        ui.derive("hi", |i| format!("Hi, {}!", i.text("name")).into());
        let frozen = ui.freeze().unwrap();
        assert_eq!(frozen.fields().text("name"), "Ada");
        assert_eq!(frozen.state("hi"), Some(&"Hi, Ada!".into()));
        assert!(!frozen.is_outdated("hi"));
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
