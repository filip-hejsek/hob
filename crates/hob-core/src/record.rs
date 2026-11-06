//! [`LogRecord`]: an error, warning or other message for the user.

use std::fmt;
use std::str::FromStr;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

/// How important a [`LogRecord`] is, most important first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// Something failed.
    Error,
    /// Something may be wrong.
    Warn,
    /// Useful to know.
    Info,
    /// Only useful for debugging. Never saved.
    Debug,
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Level::Error => "error",
            Level::Warn => "warning",
            Level::Info => "info",
            Level::Debug => "debug",
        })
    }
}

impl FromStr for Level {
    type Err = String;

    /// Parses `error`, `warn` (or `warning`), `info` or `debug`.
    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "error" => Ok(Level::Error),
            "warn" | "warning" => Ok(Level::Warn),
            "info" => Ok(Level::Info),
            "debug" => Ok(Level::Debug),
            _ => Err(format!(
                "unknown log level {s:?} (expected error, warn, info or debug)"
            )),
        }
    }
}

/// An error, warning or other message, from a command (see
/// [`Context::error`](crate::Context::error) and friends) or from elsewhere.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogRecord {
    /// How important it is.
    pub level: Level,
    /// The message.
    pub message: String,
    /// Where it came from: a command name, or a Rust module path.
    pub target: String,
    /// When it was created.
    pub time: SystemTime,
    /// Whether it's left out when saving the session. Loaded records are
    /// never ephemeral.
    #[serde(skip)]
    pub ephemeral: bool,
}

impl LogRecord {
    /// A record created now. Debug records are ephemeral; others aren't.
    pub fn new(level: Level, target: impl Into<String>, message: impl Into<String>) -> Self {
        LogRecord {
            level,
            message: message.into(),
            target: target.into(),
            time: SystemTime::now(),
            ephemeral: level == Level::Debug,
        }
    }

    /// Makes the record ephemeral: it's shown, but not saved.
    pub fn ephemeral(mut self) -> Self {
        self.ephemeral = true;
        self
    }
}

impl fmt::Display for LogRecord {
    /// `level: message`, e.g. `error: file not found`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.level, self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_parse_and_order() {
        assert_eq!("WARN".parse(), Ok(Level::Warn));
        assert_eq!("warning".parse(), Ok(Level::Warn));
        assert!("loud".parse::<Level>().is_err());
        assert!(Level::Error < Level::Warn && Level::Info < Level::Debug);
    }

    #[test]
    fn only_debug_records_are_ephemeral_by_default() {
        assert!(LogRecord::new(Level::Debug, "t", "m").ephemeral);
        assert!(!LogRecord::new(Level::Info, "t", "m").ephemeral);
        assert!(LogRecord::new(Level::Error, "t", "m").ephemeral().ephemeral);
    }
}
