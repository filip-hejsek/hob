//! The config file: TOML, read once when the shell starts. It has key
//! bindings, which change the defaults, and prompt settings:
//!
//! ```toml
//! [keys]
//! "Ctrl+Shift+L" = "clear"
//! "Ctrl+L" = false         # unbinds it
//!
//! [prompt]
//! word-chars = "_-."       # words for Ctrl+W, like zsh's WORDCHARS
//! ```

use std::fmt;
use std::path::Path;

use iced::keyboard::{Key, Modifiers, key::Named};
use serde::Deserialize;
use serde::de::{self, MapAccess, Visitor};

use crate::keys::{Action, Binding};

/// The settings read from the config file.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    /// Key bindings that replace or remove the default ones, in order.
    #[serde(default)]
    keys: Keys,
    /// Settings of the prompt.
    #[serde(default)]
    pub(crate) prompt: Prompt,
}

/// The `[prompt]` table.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct Prompt {
    /// The chars that, besides letters and digits, make up a word for
    /// `delete-word-before`, like zsh's `WORDCHARS`. Without it, a word is
    /// anything but whitespace, as in bash.
    pub(crate) word_chars: Option<String>,
}

impl Config {
    /// Reads the config file at `path`. A missing file is an empty config.
    pub(crate) fn load(path: &Path) -> Result<Self, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        Self::parse(&text).map_err(|e| format!("{}{e}", path.display()))
    }

    /// Parses a config file's text. An error starts with the `:LINE` it's
    /// on, if known, then `: ` and what's wrong.
    fn parse(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| {
            let message = e.message().trim_end();
            match e.span() {
                Some(span) => {
                    let line = text[..span.start].matches('\n').count() + 1;
                    format!(":{line}: {message}")
                }
                None => format!(": {message}"),
            }
        })
    }

    /// Changes the key `bindings` as the config says: each key it names
    /// loses its binding, and gets the one given, unless that's `false`.
    pub(crate) fn apply(&self, bindings: &mut Vec<Binding>) {
        for (key, modifiers, action) in &self.keys.0 {
            bindings.retain(|b| !b.is_for(key, *modifiers));
            if let Some(action) = action {
                bindings.push(Binding::new(key.clone(), *modifiers, *action));
            }
        }
    }
}

/// The `[keys]` table: keys with their modifiers, each with an action or
/// `None` (unbound), in the file's order.
#[derive(Debug, Default)]
struct Keys(Vec<(Key, Modifiers, Option<Action>)>);

impl<'de> Deserialize<'de> for Keys {
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct KeysVisitor;

        impl<'de> Visitor<'de> for KeysVisitor {
            type Value = Keys;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a table of keys and actions")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Keys, A::Error> {
                let mut keys = Vec::new();
                while let Some(KeyName(key, modifiers)) = map.next_key()? {
                    let ActionName(action) = map.next_value()?;
                    keys.push((key, modifiers, action));
                }
                Ok(Keys(keys))
            }
        }

        deserializer.deserialize_map(KeysVisitor)
    }
}

/// A key as written in the config, e.g. `Ctrl+Shift+Tab`.
struct KeyName(Key, Modifiers);

impl<'de> Deserialize<'de> for KeyName {
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        parse_key(&name)
            .map(|(key, modifiers)| KeyName(key, modifiers))
            .map_err(de::Error::custom)
    }
}

/// An action's name, or `false` (unbound).
struct ActionName(Option<Action>);

impl<'de> Deserialize<'de> for ActionName {
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ActionVisitor;

        impl Visitor<'_> for ActionVisitor {
            type Value = ActionName;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an action's name, or false to unbind the key")
            }

            fn visit_bool<E: de::Error>(self, v: bool) -> Result<ActionName, E> {
                if v {
                    return Err(E::invalid_value(de::Unexpected::Bool(true), &self));
                }
                Ok(ActionName(None))
            }

            fn visit_str<E: de::Error>(self, name: &str) -> Result<ActionName, E> {
                let deserializer = de::value::StrDeserializer::<de::value::Error>::new(name);
                Action::deserialize(deserializer)
                    .map(|action| ActionName(Some(action)))
                    .map_err(|_| E::custom(format!("unknown action `{name}`")))
            }
        }

        deserializer.deserialize_any(ActionVisitor)
    }
}

/// Parses a key with modifiers before it, joined by `+` (e.g. `Ctrl+L`,
/// `Shift+Tab`, `Ctrl++`). Names are compared ignoring case.
fn parse_key(name: &str) -> Result<(Key, Modifiers), String> {
    let (prefix, key) = match name.strip_suffix("++") {
        Some(prefix) => (Some(prefix), "+"),
        None if name == "+" => (None, "+"),
        None => match name.rsplit_once('+') {
            Some((prefix, key)) => (Some(prefix), key),
            None => (None, name),
        },
    };
    let mut modifiers = Modifiers::empty();
    for part in prefix.into_iter().flat_map(|p| p.split('+')) {
        let modifier = match part.to_lowercase().as_str() {
            "ctrl" | "control" => Modifiers::CTRL,
            "shift" => Modifiers::SHIFT,
            "alt" => Modifiers::ALT,
            "super" | "logo" => Modifiers::LOGO,
            _ => return Err(format!("unknown modifier `{part}` in `{name}`")),
        };
        if modifiers.contains(modifier) {
            return Err(format!("`{part}` twice in `{name}`"));
        }
        modifiers |= modifier;
    }
    let key = match named_key(key) {
        Some(named) => Key::Named(named),
        None if key.chars().count() == 1 => Key::Character(key.into()),
        None => return Err(format!("unknown key `{key}` in `{name}`")),
    };
    Ok((key, modifiers))
}

/// The named key called `name`, ignoring case.
fn named_key(name: &str) -> Option<Named> {
    let named = match name.to_lowercase().as_str() {
        "up" => Named::ArrowUp,
        "down" => Named::ArrowDown,
        "left" => Named::ArrowLeft,
        "right" => Named::ArrowRight,
        "tab" => Named::Tab,
        "space" => Named::Space,
        "enter" => Named::Enter,
        "escape" | "esc" => Named::Escape,
        "backspace" => Named::Backspace,
        "delete" => Named::Delete,
        "insert" => Named::Insert,
        "home" => Named::Home,
        "end" => Named::End,
        "pageup" => Named::PageUp,
        "pagedown" => Named::PageDown,
        "f1" => Named::F1,
        "f2" => Named::F2,
        "f3" => Named::F3,
        "f4" => Named::F4,
        "f5" => Named::F5,
        "f6" => Named::F6,
        "f7" => Named::F7,
        "f8" => Named::F8,
        "f9" => Named::F9,
        "f10" => Named::F10,
        "f11" => Named::F11,
        "f12" => Named::F12,
        _ => return None,
    };
    Some(named)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{defaults, lookup};

    fn bindings(text: &str) -> Vec<Binding> {
        let mut bindings = defaults();
        Config::parse(text).unwrap().apply(&mut bindings);
        bindings
    }

    fn char_key(c: &str) -> Key {
        Key::Character(c.into())
    }

    #[test]
    fn empty_config_keeps_defaults() {
        let bindings = bindings("");
        assert_eq!(
            lookup(&bindings, char_key("l"), Modifiers::CTRL),
            Some(Action::Clear)
        );
    }

    #[test]
    fn prompt_word_chars() {
        assert_eq!(Config::parse("").unwrap().prompt.word_chars, None);
        let config = Config::parse("[prompt]\nword-chars = \"_-\"\n").unwrap();
        assert_eq!(config.prompt.word_chars.as_deref(), Some("_-"));
        let error = Config::parse("[prompt]\nword_chars = \"_\"\n").unwrap_err();
        assert!(
            error.starts_with(":2: unknown field `word_chars`"),
            "{error}"
        );
    }

    #[test]
    fn keys_rebind_and_unbind() {
        let bindings = bindings(
            r#"
            [keys]
            "Ctrl+Shift+L" = "clear"
            "ctrl+l" = false
            "Alt+Up" = "page-up"
            "Ctrl+D" = "delete-word-before"
            "Ctrl++" = "exit"
            "#,
        );
        let ctrl_shift = Modifiers::CTRL | Modifiers::SHIFT;
        assert_eq!(
            lookup(&bindings, char_key("L"), ctrl_shift),
            Some(Action::Clear)
        );
        assert_eq!(lookup(&bindings, char_key("l"), Modifiers::CTRL), None);
        assert_eq!(
            lookup(&bindings, Key::Named(Named::ArrowUp), Modifiers::ALT),
            Some(Action::PageUp)
        );
        assert_eq!(
            lookup(&bindings, Key::Named(Named::ArrowUp), Modifiers::empty()),
            Some(Action::Up)
        );
        assert_eq!(
            lookup(&bindings, char_key("d"), Modifiers::CTRL),
            Some(Action::DeleteWordBefore)
        );
        assert_eq!(
            lookup(&bindings, char_key("+"), Modifiers::CTRL),
            Some(Action::Exit)
        );
    }

    #[test]
    fn errors_say_where() {
        let error = |text| Config::parse(text).unwrap_err();
        assert_eq!(
            error("[keys]\n\"Ctrl+L\" = \"clean\""),
            ":2: unknown action `clean`"
        );
        assert_eq!(
            error("[keys]\n\"Hyper+L\" = \"clear\""),
            ":2: unknown modifier `Hyper` in `Hyper+L`"
        );
        assert_eq!(
            error("[keys]\n\"Ctrl+Foo\" = \"clear\""),
            ":2: unknown key `Foo` in `Ctrl+Foo`"
        );
        assert_eq!(
            error("[keys]\n\"Ctrl+Ctrl+L\" = \"clear\""),
            ":2: `Ctrl` twice in `Ctrl+Ctrl+L`"
        );
        assert!(
            error("[keys]\n\"Ctrl+L\" = true").starts_with(":2: invalid value: boolean `true`")
        );
        assert!(error("[colours]").starts_with(":1: unknown field `colours`"));
    }
}
