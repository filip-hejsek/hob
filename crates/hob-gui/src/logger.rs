//! A `log` backend that sends records to the shell, which shows them as
//! notices.

use std::collections::HashSet;
use std::sync::Mutex;

use futures_channel::mpsc;
use hob_core::{Level, LogRecord};

/// Sends [`log`] records to the shell. Records of our crates are logged
/// from `level` up; records of dependencies only from warnings up, and they
/// are ephemeral, and logged only once. Errors and warnings also go to stderr.
struct Logger {
    level: Level,
    sender: mpsc::UnboundedSender<LogRecord>,
    /// Dependency records already logged: level, target and message.
    seen: Mutex<HashSet<(Level, String, String)>>,
}

/// Installs the logger, if no other logger is installed yet. Returns the
/// records it receives.
pub(crate) fn install(level: Level) -> Option<mpsc::UnboundedReceiver<LogRecord>> {
    let (sender, receiver) = mpsc::unbounded();
    let logger = Logger {
        level,
        sender,
        seen: Mutex::default(),
    };
    log::set_boxed_logger(Box::new(logger)).ok()?;
    log::set_max_level(filter(level.max(Level::Warn)));
    Some(receiver)
}

/// Whether a target (module path) is in one of our crates.
pub(crate) fn is_ours(target: &str) -> bool {
    let krate = target.split("::").next().unwrap_or(target);
    ["hob", "kettle_ui"]
        .iter()
        .any(|ours| krate == *ours || krate.strip_prefix(ours).is_some_and(|r| r.starts_with('_')))
}

fn level(level: log::Level) -> Level {
    match level {
        log::Level::Error => Level::Error,
        log::Level::Warn => Level::Warn,
        log::Level::Info => Level::Info,
        log::Level::Debug | log::Level::Trace => Level::Debug,
    }
}

fn filter(level: Level) -> log::LevelFilter {
    match level {
        Level::Error => log::LevelFilter::Error,
        Level::Warn => log::LevelFilter::Warn,
        Level::Info => log::LevelFilter::Info,
        Level::Debug => log::LevelFilter::Debug,
    }
}

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        let max = if is_ours(metadata.target()) {
            self.level
        } else {
            Level::Warn
        };
        level(metadata.level()) <= max
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let target = record.target();
        let mut entry = LogRecord::new(level(record.level()), target, record.args().to_string());
        if !is_ours(target) {
            // Some dependencies repeat a warning, e.g. on every frame.
            let key = (entry.level, entry.target.clone(), entry.message.clone());
            if !self.seen.lock().unwrap().insert(key) {
                return;
            }
            entry = entry.ephemeral();
        }
        if entry.level <= Level::Warn {
            eprintln!("hob: {entry} ({target})");
        }
        // Only fails once the shell is gone.
        let _ = self.sender.unbounded_send(entry);
    }

    fn flush(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_our_crates() {
        for ours in ["hob", "hob_gui::logger", "kettle_ui", "kettle_ui_iced::x"] {
            assert!(is_ours(ours), "{ours}");
        }
        for theirs in ["hobby", "iced_winit::proxy", "wgpu_core", "kettle"] {
            assert!(!is_ours(theirs), "{theirs}");
        }
    }

    #[test]
    fn repeated_dependency_records_are_dropped() {
        let (sender, mut receiver) = mpsc::unbounded();
        let logger = Logger {
            level: Level::Info,
            sender,
            seen: Mutex::default(),
        };
        let log = |target, message| {
            let args = format_args!("{message}");
            let record = log::Record::builder()
                .level(log::Level::Error)
                .target(target)
                .args(args)
                .build();
            log::Log::log(&logger, &record);
        };
        for _ in 0..2 {
            log("wgpu_core", "lost");
            log("hob_gui", "ours");
        }
        log("wgpu_core", "other");
        let mut received = Vec::new();
        while let Ok(record) = receiver.try_recv() {
            received.push((record.message, record.ephemeral));
        }
        let expected = [
            ("lost", true),
            ("ours", false),
            ("ours", false),
            ("other", true),
        ];
        assert_eq!(received, expected.map(|(m, e)| (m.to_owned(), e)));
    }
}
