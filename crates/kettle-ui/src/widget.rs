//! The widgets an island is built from.
//!
//! Build the (fixed) widget tree with the functions in this module:
//! - layout: [`column()`], [`row`], [`when`]
//! - display: [`label`] (with [`state`]), [`progress`], [`list`]
//! - form elements: [`text_field`], [`text_area`], [`checkbox`], [`suggestions`]
//! - buttons: [`submit_button`], [`cancel_button`] (with [`ButtonLabel`])
//!
//! Backends read the tree through [`Widget::kind`], and leave out the gaps
//! around widgets that have nothing to show (see [`is_visible`]).

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::{Contents, Value};

/// One node of the (immutable) widget tree. Build it with the free functions
/// in [this module](self): [`column()`], [`label`], [`text_field`], ...
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub struct Widget(Kind);

impl Widget {
    /// What this widget is, for backends to render it.
    pub fn kind(&self) -> &Kind {
        &self.0
    }
}

/// What a [`Widget`] is. See the function that creates each kind for its
/// meaning.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// [`column()`]
    Column(Vec<Widget>),
    /// [`row`]
    Row(Vec<Widget>),
    /// [`when`]
    When {
        /// The named state that decides whether `child` is shown.
        state: String,
        /// The widget shown while `state` is true.
        child: Box<Widget>,
    },
    /// [`label`]
    Label(Text),
    /// [`text_field`]
    TextField {
        /// The field's id in [`Fields`](crate::Fields).
        id: Arc<str>,
        /// Shown while the field is empty.
        placeholder: String,
    },
    /// [`text_area`]
    TextArea {
        /// The field's id in [`Fields`](crate::Fields).
        id: Arc<str>,
        /// Shown while the field is empty.
        placeholder: String,
    },
    /// [`checkbox`]
    Checkbox {
        /// The field's id in [`Fields`](crate::Fields).
        id: Arc<str>,
        /// Shown next to the checkbox.
        label: String,
    },
    /// [`progress`]
    Progress {
        /// The named state shown.
        state: String,
    },
    /// [`list`]
    List {
        /// The named state shown.
        state: String,
    },
    /// [`suggestions`]
    Suggestions {
        /// The named state shown.
        state: String,
        /// The id of the text field a clicked line goes into.
        field: Arc<str>,
    },
    /// [`submit_button`] or [`cancel_button`]
    Button {
        /// Shown on the button.
        label: String,
        /// The key that presses it, if any (see [`ButtonLabel`]).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<char>,
        /// What clicking it does.
        action: Action,
    },
}

/// What a button does.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// [`submit_button`]
    Submit,
    /// [`cancel_button`]
    Cancel,
}

/// Text shown by a [`label`]: either fixed, or bound to named state.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Text {
    /// Fixed text.
    Static(String),
    /// The value of the named state with this name.
    State(String),
}

impl From<&str> for Text {
    fn from(s: &str) -> Self {
        Text::Static(s.to_owned())
    }
}

impl From<String> for Text {
    fn from(s: String) -> Self {
        Text::Static(s)
    }
}

/// Refers to named state, for use in [`label`].
pub fn state(name: &str) -> Text {
    Text::State(name.to_owned())
}

/// Lays out children vertically.
pub fn column(children: impl IntoIterator<Item = Widget>) -> Widget {
    Widget(Kind::Column(children.into_iter().collect()))
}

/// Lays out children horizontally.
pub fn row(children: impl IntoIterator<Item = Widget>) -> Widget {
    Widget(Kind::Row(children.into_iter().collect()))
}

/// Shows `child` only while the named state `state` is
/// [`Value::Bool`]`(true)`. A hidden widget can't be used and takes no space,
/// but keeps its field value.
pub fn when(state: &str, child: Widget) -> Widget {
    Widget(Kind::When {
        state: state.to_owned(),
        child: Box::new(child),
    })
}

/// Shows fixed text, or the value of named state (see [`state`]).
pub fn label(text: impl Into<Text>) -> Widget {
    Widget(Kind::Label(text.into()))
}

/// A single-line text input. Its value is read with [`Fields::text`](crate::Fields::text).
pub fn text_field(id: &str, placeholder: &str) -> Widget {
    Widget(Kind::TextField {
        id: id.into(),
        placeholder: placeholder.to_owned(),
    })
}

/// A multi-line text input, for longer text. Its value is read with
/// [`Fields::text`](crate::Fields::text), like a [`text_field`]'s, and it
/// counts as a text field wherever one is expected.
pub fn text_area(id: &str, placeholder: &str) -> Widget {
    Widget(Kind::TextArea {
        id: id.into(),
        placeholder: placeholder.to_owned(),
    })
}

/// A checkbox. Its value is read with [`Fields::checked`](crate::Fields::checked).
pub fn checkbox(id: &str, label: &str) -> Widget {
    Widget(Kind::Checkbox {
        id: id.into(),
        label: label.to_owned(),
    })
}

/// A progress bar showing named state as a number from 0 to 1.
pub fn progress(state: &str) -> Widget {
    Widget(Kind::Progress {
        state: state.to_owned(),
    })
}

/// A list of lines showing named state (usually a [`Value::List`]).
pub fn list(state: &str) -> Widget {
    Widget(Kind::List {
        state: state.to_owned(),
    })
}

/// Like [`list`], but clicking a line puts it into the text field `field`.
/// For autocomplete.
pub fn suggestions(state: &str, field: &str) -> Widget {
    Widget(Kind::Suggestions {
        state: state.to_owned(),
        field: field.into(),
    })
}

/// The label of a button, and the key that presses it.
///
/// A fixed label can give the key at its end: `"Yes [y]"` is the label
/// `"Yes"` with the key `y`. Labels made at runtime aren't parsed: build
/// them with [`ButtonLabel::new`] (and [`ButtonLabel::key`]). That's also
/// the way to a label that ends with a bracketed letter.
///
/// A key is a lowercase ASCII letter. It works while the island has focus,
/// and only in islands without text fields; no two buttons of an island can
/// have the same key (see [`IslandBuilder::build`](crate::IslandBuilder::build)).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ButtonLabel {
    text: String,
    key: Option<char>,
}

impl ButtonLabel {
    /// A label shown as it is, without a key.
    pub fn new(text: impl Into<String>) -> Self {
        ButtonLabel {
            text: text.into(),
            key: None,
        }
    }

    /// Sets the key that presses the button.
    pub fn key(mut self, key: char) -> Self {
        self.key = Some(key);
        self
    }
}

/// Parses a key at the end, like `"Yes [y]"`.
impl From<&'static str> for ButtonLabel {
    fn from(label: &'static str) -> Self {
        let parsed = label.strip_suffix(']').and_then(|rest| {
            let (text, key) = rest.rsplit_once('[')?;
            let mut chars = key.chars();
            match (chars.next(), chars.next()) {
                (Some(key), None) => Some(ButtonLabel::new(text.trim_end()).key(key)),
                _ => None,
            }
        });
        parsed.unwrap_or_else(|| ButtonLabel::new(label))
    }
}

/// Submits the form: the island reports [`Outcome::Submitted`](crate::Outcome::Submitted) with the
/// entered values.
pub fn submit_button(label: impl Into<ButtonLabel>) -> Widget {
    button(label.into(), Action::Submit)
}

/// The island reports [`Outcome::Cancelled`](crate::Outcome::Cancelled).
pub fn cancel_button(label: impl Into<ButtonLabel>) -> Widget {
    button(label.into(), Action::Cancel)
}

fn button(label: ButtonLabel, action: Action) -> Widget {
    Widget(Kind::Button {
        label: label.text,
        key: label.key,
        action,
    })
}

/// Whether `widget` has anything to show. Backends give a hidden widget no
/// gaps around it.
///
/// Hidden are: a label with empty text (missing state counts as empty), a
/// list or suggestions with no items (or whose state isn't a list), a column
/// or row whose children are all hidden, and a [`when`] whose state isn't
/// true or whose child is hidden. Other widgets are always visible.
pub fn is_visible(contents: &impl Contents, widget: &Widget) -> bool {
    match widget.kind() {
        Kind::Column(children) | Kind::Row(children) => {
            children.iter().any(|c| is_visible(contents, c))
        }
        Kind::When { state, child } => is_shown(contents, state) && is_visible(contents, child),
        Kind::Label(Text::Static(s)) => !s.is_empty(),
        Kind::Label(Text::State(name)) => contents
            .state(name)
            .is_some_and(|v| !v.to_string().is_empty()),
        Kind::List { state } | Kind::Suggestions { state, .. } => {
            matches!(contents.state(state), Some(Value::List(items)) if !items.is_empty())
        }
        Kind::TextField { .. }
        | Kind::TextArea { .. }
        | Kind::Checkbox { .. }
        | Kind::Progress { .. }
        | Kind::Button { .. } => true,
    }
}

/// Calls `f` with the key and action of each button that has a key, in tree
/// order.
pub(crate) fn keys(widget: &Widget, f: &mut impl FnMut(char, Action)) {
    match widget.kind() {
        Kind::Column(children) | Kind::Row(children) => {
            children.iter().for_each(|c| keys(c, f));
        }
        Kind::When { child, .. } => keys(child, f),
        Kind::Button {
            key: Some(key),
            action,
            ..
        } => f(*key, *action),
        _ => {}
    }
}

/// Whether a [`when`] with this state shows its child.
pub fn is_shown(contents: &impl Contents, state: &str) -> bool {
    contents.state(state) == Some(&Value::Bool(true))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::Fields;

    struct TestContents {
        root: Widget,
        fields: Fields,
        state: HashMap<String, Value>,
    }

    impl Contents for TestContents {
        fn root(&self) -> &Widget {
            &self.root
        }
        fn fields(&self) -> &Fields {
            &self.fields
        }
        fn state(&self, name: &str) -> Option<&Value> {
            self.state.get(name)
        }
        fn is_outdated(&self, _: &str) -> bool {
            false
        }
    }

    fn contents(state: impl IntoIterator<Item = (&'static str, Value)>) -> TestContents {
        TestContents {
            root: column([]),
            fields: Fields::default(),
            state: state.into_iter().map(|(k, v)| (k.to_owned(), v)).collect(),
        }
    }

    #[test]
    fn labels() {
        let c = contents([
            ("text", "a".into()),
            ("empty", "".into()),
            ("zero", 0.0.into()),
            ("no_items", Value::List(Vec::new())),
        ]);
        assert!(is_visible(&c, &label("a")));
        assert!(!is_visible(&c, &label("")));
        assert!(is_visible(&c, &label(state("text"))));
        assert!(is_visible(&c, &label(state("zero"))));
        assert!(!is_visible(&c, &label(state("empty"))));
        assert!(!is_visible(&c, &label(state("missing"))));
        assert!(!is_visible(&c, &label(state("no_items"))));
    }

    #[test]
    fn lists() {
        let c = contents([
            ("items", Value::List(vec!["a".into()])),
            ("no_items", Value::List(Vec::new())),
            ("text", "a".into()),
        ]);
        assert!(is_visible(&c, &list("items")));
        assert!(is_visible(&c, &suggestions("items", "f")));
        for name in ["no_items", "text", "missing"] {
            assert!(!is_visible(&c, &list(name)));
            assert!(!is_visible(&c, &suggestions(name, "f")));
        }
    }

    #[test]
    fn other_widgets_are_visible() {
        let c = contents([]);
        assert!(is_visible(&c, &text_field("f", "")));
        assert!(is_visible(&c, &text_area("f", "")));
        assert!(is_visible(&c, &checkbox("c", "")));
        assert!(is_visible(&c, &progress("missing")));
        assert!(is_visible(&c, &submit_button("")));
    }

    #[test]
    fn button_keys_are_parsed_from_fixed_labels() {
        let parsed = |label: &'static str| ButtonLabel::from(label);
        assert_eq!(parsed("Yes [y]"), ButtonLabel::new("Yes").key('y'));
        assert_eq!(parsed("[n]"), ButtonLabel::new("").key('n'));
        assert_eq!(parsed("No[N]"), ButtonLabel::new("No").key('N'));
        for plain in ["OK", "A [bc]", "A []", "A [y] ", "]"] {
            assert_eq!(parsed(plain), ButtonLabel::new(plain));
        }
    }

    #[test]
    fn nested_layouts() {
        let c = contents([("items", Value::List(vec!["a".into()]))]);
        assert!(!is_visible(&c, &column([])));
        assert!(!is_visible(
            &c,
            &column([label(""), row([list("missing")])])
        ));
        assert!(is_visible(&c, &column([label(""), row([list("items")])])));
        assert!(is_visible(&c, &row([column([]), text_field("f", "")])));
    }

    #[test]
    fn when_shows_its_child_only_while_true() {
        let c = contents([
            ("yes", true.into()),
            ("no", false.into()),
            ("text", "true".into()),
        ]);
        assert!(is_visible(&c, &when("yes", text_field("f", ""))));
        assert!(!is_visible(&c, &when("yes", label(""))));
        for name in ["no", "text", "missing"] {
            assert!(!is_visible(&c, &when(name, text_field("f", ""))));
        }
    }
}
