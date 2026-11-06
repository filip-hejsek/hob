//! The widgets an island is built from.
//!
//! Build the (fixed) widget tree with the functions in this module:
//! - layout: [`column()`], [`row`]
//! - display: [`label`] (with [`state`]), [`progress`], [`list`]
//! - form elements: [`text_field`], [`checkbox`], [`suggestions`]
//! - buttons: [`submit_button`], [`cancel_button`]
//!
//! Backends read the tree through [`Widget::kind`].

use std::sync::Arc;

use serde::{Deserialize, Serialize};

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
    /// [`label`]
    Label(Text),
    /// [`text_field`]
    TextField {
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

/// A list of lines showing named state (usually a [`Value::List`](crate::Value::List)).
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

/// Submits the form: the island reports [`Outcome::Submitted`](crate::Outcome::Submitted) with the
/// entered values.
pub fn submit_button(label: &str) -> Widget {
    Widget(Kind::Button {
        label: label.to_owned(),
        action: Action::Submit,
    })
}

/// The island reports [`Outcome::Cancelled`](crate::Outcome::Cancelled).
pub fn cancel_button(label: &str) -> Widget {
    Widget(Kind::Button {
        label: label.to_owned(),
        action: Action::Cancel,
    })
}
