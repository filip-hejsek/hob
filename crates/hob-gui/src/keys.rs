//! The shell's key bindings: a table from keys to [`Action`]s, kept apart
//! from what the actions do, so it can become configurable.

use iced::keyboard::{Key, Modifiers, key::Named};

/// Something a key binding does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// The previous line in the history.
    HistoryBack,
    /// The next line in the history.
    HistoryForward,
    /// Completes the word being typed.
    Complete,
    /// Scrolls the scrollback up by a page.
    PageUp,
    /// Scrolls the scrollback down by a page.
    PageDown,
    /// Clears the scrollback, like the `clear` command.
    Clear,
    /// Exits if the prompt is empty and no command is running.
    Exit,
}

/// Where focus must be for an [`Action`] to work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scope {
    /// Anywhere.
    Anywhere,
    /// In the prompt.
    Prompt,
    /// In the prompt, or nowhere.
    PromptOrNothing,
}

impl Action {
    /// Where focus must be for it to work.
    pub(crate) fn scope(self) -> Scope {
        match self {
            Action::HistoryBack | Action::HistoryForward | Action::Complete => Scope::Prompt,
            Action::Exit => Scope::PromptOrNothing,
            Action::PageUp | Action::PageDown | Action::Clear => Scope::Anywhere,
        }
    }
}

/// A key, with exactly these modifiers, bound to an action.
#[derive(Clone, Debug)]
pub(crate) struct Binding {
    key: Key,
    modifiers: Modifiers,
    action: Action,
}

impl Binding {
    fn new(key: Key, modifiers: Modifiers, action: Action) -> Self {
        Binding {
            key: normalize(key),
            modifiers,
            action,
        }
    }
}

/// The default bindings.
pub(crate) fn defaults() -> Vec<Binding> {
    let named = |key, action| Binding::new(Key::Named(key), Modifiers::empty(), action);
    let ctrl = |c: &str, action| Binding::new(Key::Character(c.into()), Modifiers::CTRL, action);
    vec![
        named(Named::ArrowUp, Action::HistoryBack),
        named(Named::ArrowDown, Action::HistoryForward),
        named(Named::Tab, Action::Complete),
        named(Named::PageUp, Action::PageUp),
        named(Named::PageDown, Action::PageDown),
        ctrl("l", Action::Clear),
        ctrl("d", Action::Exit),
    ]
}

/// The action bound to `key` with `modifiers`, if any.
pub(crate) fn lookup(bindings: &[Binding], key: Key, modifiers: Modifiers) -> Option<Action> {
    let key = normalize(key);
    (bindings.iter())
        .find(|b| b.key == key && b.modifiers == modifiers)
        .map(|b| b.action)
}

/// Letters in lowercase, so caps lock doesn't matter.
fn normalize(key: Key) -> Key {
    match key {
        Key::Character(c) => Key::Character(c.to_lowercase().into()),
        key => key,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_matches_key_and_exact_modifiers() {
        let bindings = defaults();
        let l = || Key::Character("l".into());
        assert_eq!(lookup(&bindings, l(), Modifiers::CTRL), Some(Action::Clear));
        assert_eq!(
            lookup(&bindings, Key::Character("L".into()), Modifiers::CTRL),
            Some(Action::Clear)
        );
        assert_eq!(lookup(&bindings, l(), Modifiers::empty()), None);
        assert_eq!(
            lookup(&bindings, Key::Character("d".into()), Modifiers::CTRL),
            Some(Action::Exit)
        );
        assert_eq!(
            lookup(&bindings, l(), Modifiers::CTRL | Modifiers::SHIFT),
            None
        );
        assert_eq!(
            lookup(&bindings, Key::Named(Named::Tab), Modifiers::empty()),
            Some(Action::Complete)
        );
        assert_eq!(
            lookup(&bindings, Key::Named(Named::Tab), Modifiers::SHIFT),
            None
        );
    }
}
