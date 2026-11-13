//! The shell's key bindings: a table from keys to [`Action`]s, kept apart
//! from what the actions do, so it can become configurable.

use iced::keyboard::{Key, Modifiers, key::Named};

/// Something a key binding does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// Picks the candidate above the picked one, while picking; otherwise
    /// the previous line in the history.
    Up,
    /// Picks the candidate below the picked one, while picking; otherwise
    /// the next line in the history.
    Down,
    /// Picks the previous candidate, while picking; otherwise moves the
    /// cursor left.
    Left,
    /// Picks the next candidate, while picking; otherwise moves the cursor
    /// right.
    Right,
    /// Completes the word being typed; once candidates are listed, picks
    /// the next one.
    Complete,
    /// Picks the previous candidate, once candidates are listed.
    PickPrevious,
    /// Stops picking a candidate, restoring the line from before.
    CancelPick,
    /// Takes the picked candidate bare, or cycles the endings of the one
    /// taken; otherwise types a space.
    TakePick,
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
    /// In the prompt (its key bindings do it).
    Prompt,
    /// In the prompt, or nowhere (then the shell's subscription does it).
    PromptOrNothing,
}

impl Action {
    /// Where focus must be for it to work.
    pub(crate) fn scope(self) -> Scope {
        match self {
            Action::Up
            | Action::Down
            | Action::Left
            | Action::Right
            | Action::Complete
            | Action::PickPrevious
            | Action::TakePick => Scope::Prompt,
            // Escape also restores the line after a click elsewhere.
            Action::Exit | Action::CancelPick => Scope::PromptOrNothing,
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
    let shift = |key, action| Binding::new(Key::Named(key), Modifiers::SHIFT, action);
    let ctrl = |c: &str, action| Binding::new(Key::Character(c.into()), Modifiers::CTRL, action);
    vec![
        named(Named::ArrowUp, Action::Up),
        named(Named::ArrowDown, Action::Down),
        named(Named::ArrowLeft, Action::Left),
        named(Named::ArrowRight, Action::Right),
        named(Named::Tab, Action::Complete),
        shift(Named::Tab, Action::PickPrevious),
        shift(Named::Space, Action::TakePick),
        named(Named::Escape, Action::CancelPick),
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

/// The key to look up in the bindings: the key without modifiers, so
/// that e.g. Ctrl+Shift+L is still an L, except where the modifiers turned
/// a named key into a character, as Num Lock does with the numpad's arrows.
pub(crate) fn bound_key(key: Key, modified_key: Key) -> Key {
    match (&key, &modified_key) {
        (Key::Named(_), Key::Character(_)) => modified_key,
        _ => key,
    }
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
            Some(Action::PickPrevious)
        );
        assert_eq!(
            lookup(&bindings, Key::Named(Named::Tab), Modifiers::CTRL),
            None
        );
        assert_eq!(
            lookup(&bindings, Key::Named(Named::Space), Modifiers::SHIFT),
            Some(Action::TakePick)
        );
    }
}
