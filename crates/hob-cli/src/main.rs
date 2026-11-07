//! A demo client for the headless hob frontend: an interactive command line.
//!
//! It starts the frontend (`hob --headless` from the same directory, or the
//! command given as arguments; see [`Client::command`]) and talks to it over
//! JSON-RPC. Lines typed
//! are run as hob commands; lines starting with `/` act on islands (see
//! [`HELP`]). Everything the commands do is printed as it arrives, prefixed
//! with the command's block number; islands are printed as text outlines.
//!
//! The `exit` command quits, like `/quit`.
//!
//! Output from commands can arrive while the user types. As a stopgap, the
//! `> ` prompt is only shown once output has paused for [`PROMPT_DELAY`], and
//! not at all if stdin isn't a terminal.

use std::collections::BTreeMap;
use std::io::{self, BufRead, IsTerminal, Write};
use std::process::ExitCode;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use hob_core::Level;
use hob_headless::client::Client;
use hob_headless::protocol::{BlockId, Call, IslandId, Notification};
use kettle_ui::FrozenIsland;

const HELP: &str = "\
Lines are run as hob commands (try help). Driver commands act on the newest
live island, or on island N if given as #N:
  /type [#N] FIELD TEXT   set the text of a text field
  /check [#N] FIELD       check a checkbox (/uncheck to uncheck)
  /pick [#N] FIELD LINE   pick a suggestion shown for a text field
  /press [#N] LABEL       press a button
  /show [#N]              print an island again
  /help                   show this
  /quit                   quit (or end the input)";

/// How long output must pause before the prompt is shown.
const PROMPT_DELAY: Duration = Duration::from_millis(50);

/// Prints a line of output; see [`Output::line`].
macro_rules! out {
    ($($arg:tt)*) => {
        OUTPUT.line(&format!($($arg)*))
    };
}

static OUTPUT: Output = Output {
    state: Mutex::new(OutputState {
        last: None,
        waiting: false,
        shown: false,
    }),
    changed: Condvar::new(),
};

/// All output, and the prompt.
struct Output {
    state: Mutex<OutputState>,
    changed: Condvar,
}

struct OutputState {
    /// When output was last printed, or waiting started.
    last: Option<Instant>,
    /// Whether the user is expected to type a line.
    waiting: bool,
    /// Whether the prompt is shown, at the end of the last line.
    shown: bool,
}

impl Output {
    /// Prints a line, over the prompt if it's shown.
    fn line(&self, text: &str) {
        let mut state = self.state.lock().unwrap();
        if state.shown {
            print!("\r");
            state.shown = false;
        }
        println!("{text}");
        state.last = Some(Instant::now());
        self.changed.notify_all();
    }

    /// Clears the terminal, with its scrollback, for the `clear` command of
    /// `block`. If stdout isn't a terminal, only says so.
    fn clear(&self, block: BlockId) {
        if !io::stdout().is_terminal() {
            return self.line(&format!("[{block}] cleared"));
        }
        let mut state = self.state.lock().unwrap();
        // Scrollback, screen, then the cursor to the top left.
        print!("\x1b[3J\x1b[2J\x1b[H");
        let _ = io::stdout().flush();
        state.shown = false;
        state.last = Some(Instant::now());
        self.changed.notify_all();
    }

    /// Says whether the user is expected to type a line. When a line has been
    /// typed, the prompt it followed is gone. The delay starts again, so that
    /// output of a command that was just started comes first.
    fn set_waiting(&self, waiting: bool) {
        let mut state = self.state.lock().unwrap();
        state.waiting = waiting;
        state.shown = false;
        state.last = Some(Instant::now());
        self.changed.notify_all();
    }

    /// Ends the line after the prompt, if it's shown.
    fn end(&self) {
        let mut state = self.state.lock().unwrap();
        if state.shown {
            println!();
            state.shown = false;
        }
    }

    /// Shows the prompt while the user is expected to type a line and output
    /// has paused for [`PROMPT_DELAY`]. Never returns.
    fn show_prompts(&self) {
        let mut state = self.state.lock().unwrap();
        loop {
            if state.waiting && !state.shown {
                let quiet = state.last.map_or(PROMPT_DELAY, |last| last.elapsed());
                if quiet < PROMPT_DELAY {
                    state = self
                        .changed
                        .wait_timeout(state, PROMPT_DELAY - quiet)
                        .unwrap()
                        .0;
                    continue;
                }
                print!("> ");
                let _ = io::stdout().flush();
                state.shown = true;
            }
            state = self.changed.wait(state).unwrap();
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("hob-cli: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let command =
        Client::command(std::env::args_os().skip(1)).map_err(|e| format!("can't find hob: {e}"))?;
    let description = format!("{command:?}");
    // Notifications and errors of calls are printed by one thread, in order.
    let (printer, printed) = mpsc::channel();
    let notifications = printer.clone();
    let notify = move |n| {
        let _ = notifications.send(Print::Notification(n));
    };
    let mut client =
        Client::spawn(command, notify).map_err(|e| format!("couldn't start {description}: {e}"))?;

    let state = Arc::new(Mutex::new(State::default()));
    let printer_state = state.clone();
    thread::spawn(move || print(&printer_state, &printed));

    if io::stdin().is_terminal() {
        thread::spawn(|| OUTPUT.show_prompts());
    }
    out!("{HELP}");
    repl(&mut client, &state, &printer)
    // Dropping the client makes the frontend exit.
}

/// Something for the printing thread.
enum Print {
    Notification(Notification),
    /// The error a call failed with.
    Error(String),
}

/// What the client knows about islands.
#[derive(Default)]
struct State {
    /// The last contents of each island, and whether it's live.
    islands: BTreeMap<IslandId, (BlockId, FrozenIsland, bool)>,
}

impl State {
    /// The island `#N`, or the newest live one.
    fn island(&self, id: Option<IslandId>) -> Result<IslandId, String> {
        match id {
            Some(id) => Ok(id),
            None => (self.islands.iter().rev())
                .find(|(_, (_, _, live))| *live)
                .map(|(id, _)| *id)
                .ok_or_else(|| "no live island".to_owned()),
        }
    }
}

fn repl(client: &mut Client, state: &Mutex<State>, printer: &Sender<Print>) -> Result<(), String> {
    let mut lines = io::stdin().lock().lines();
    loop {
        OUTPUT.set_waiting(true);
        let line = lines.next();
        OUTPUT.set_waiting(false);
        let Some(line) = line else {
            OUTPUT.end();
            return Ok(());
        };
        let line = line.map_err(|e| format!("couldn't read input: {e}"))?;
        let call = match parse(&line) {
            Ok(Input::Nothing) => continue,
            Ok(Input::Quit) => return Ok(()),
            Ok(Input::Help) => {
                out!("{HELP}");
                continue;
            }
            Ok(Input::Run(line)) => Call::Run { line },
            Ok(Input::Show(id)) => {
                let state = state.lock().unwrap();
                match state.island(id) {
                    Ok(id) => match state.islands.get(&id) {
                        Some((block, contents, live)) => {
                            print_island(*block, id, contents, *live);
                        }
                        None => out!("no island #{id}"),
                    },
                    Err(e) => out!("{e}"),
                }
                continue;
            }
            Ok(Input::Act(id, action)) => match state.lock().unwrap().island(id) {
                Ok(island) => action.call(island),
                Err(e) => {
                    out!("{e}");
                    continue;
                }
            },
            Err(e) => {
                out!("{e}");
                continue;
            }
        };
        let result = client.call(&call).map_err(|e| e.to_string())?;
        if let Err(error) = result {
            // After the notifications the call caused.
            let _ = printer.send(Print::Error(error.message));
        }
    }
}

/// Prints notifications and errors as they arrive.
fn print(state: &Mutex<State>, printed: &Receiver<Print>) {
    let level = hob_core::logger::level_from_env();
    for print in printed {
        match print {
            Print::Notification(n) => print_notification(n, state, level),
            Print::Error(message) => out!("error: {message}"),
        }
    }
}

fn print_notification(notification: Notification, state: &Mutex<State>, level: Level) {
    match notification {
        Notification::Printed { block, text } => {
            let lines: Vec<String> = text.lines().map(|l| format!("[{block}] {l}")).collect();
            out!("{}", lines.join("\n"));
        }
        Notification::Logged { block, record } => {
            if record.level <= level {
                let block = block.map_or("-".to_owned(), |b| b.to_string());
                out!("[{block}] {record}");
            }
        }
        Notification::Island {
            block,
            island,
            live,
            contents,
        } => {
            let mut state = state.lock().unwrap();
            match state.islands.get(&island) {
                Some(_) if !live => out!("[{block}] island #{island} closed"),
                Some((_, old, _)) => {
                    let old = kettle_ui_headless::outline(old);
                    let new = kettle_ui_headless::outline(&contents);
                    match changed_lines(&old, &new) {
                        Some(lines) => {
                            for line in lines {
                                out!("[{block}] island #{island}: {}", line.trim());
                            }
                        }
                        None => print_island(block, island, &contents, live),
                    }
                }
                None => print_island(block, island, &contents, live),
            }
            state.islands.insert(island, (block, contents, live));
        }
        Notification::Cleared { block } => OUTPUT.clear(block),
        Notification::ExitRequested { .. } => {
            OUTPUT.end();
            // The main thread may be waiting for input. Exiting ends the
            // frontend's input, so it exits too.
            std::process::exit(0);
        }
        Notification::Finished { block } => out!("[{block}] done"),
    }
}

/// The lines of `new` that differ from `old`, if both have the same number of
/// lines; `None` if lines were added or removed.
fn changed_lines<'a>(old: &str, new: &'a str) -> Option<Vec<&'a str>> {
    if old.lines().count() != new.lines().count() {
        return None;
    }
    let pairs = old.lines().zip(new.lines());
    Some(pairs.filter(|(o, n)| o != n).map(|(_, n)| n).collect())
}

fn print_island(block: BlockId, island: IslandId, contents: &FrozenIsland, live: bool) {
    let kind = if live { "" } else { " (frozen)" };
    let mut text = format!("[{block}] island #{island}{kind}:");
    for line in kettle_ui_headless::outline(contents).lines() {
        text += &format!("\n[{block}]   {line}");
    }
    out!("{text}");
}

/// A line typed by the user.
#[derive(Debug, PartialEq, Eq)]
enum Input {
    Nothing,
    Run(String),
    Act(Option<IslandId>, Action),
    Show(Option<IslandId>),
    Help,
    Quit,
}

/// An action on an island.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    Type { field: String, text: String },
    SetChecked { field: String, checked: bool },
    Pick { field: String, line: String },
    Press { label: String },
}

impl Action {
    fn call(self, island: IslandId) -> Call {
        match self {
            Action::Type { field, text } => Call::TypeText {
                island,
                field,
                text,
            },
            Action::SetChecked { field, checked } => Call::SetChecked {
                island,
                field,
                checked,
            },
            Action::Pick { field, line } => Call::Pick {
                island,
                field,
                line,
            },
            Action::Press { label } => Call::Press { island, label },
        }
    }
}

fn parse(line: &str) -> Result<Input, String> {
    let line = line.trim();
    let Some(command) = line.strip_prefix('/') else {
        return Ok(match line {
            "" => Input::Nothing,
            _ => Input::Run(line.to_owned()),
        });
    };
    let (name, rest) = split_word(command);
    let (island, rest) = match split_word(rest) {
        (word, rest) if word.starts_with('#') => {
            let id = word[1..]
                .parse()
                .map_err(|_| format!("bad island {word}"))?;
            (Some(id), rest)
        }
        _ => (None, rest),
    };
    let usage = |usage: &str| Err(format!("usage: /{name} {usage}"));
    let field = |usage| match split_word(rest) {
        ("", _) => Err(usage),
        (field, rest) => Ok((field.to_owned(), rest.to_owned())),
    };
    let action = match name {
        "type" => match field("[#N] FIELD TEXT") {
            Ok((field, text)) => Action::Type { field, text },
            Err(u) => return usage(u),
        },
        "check" | "uncheck" => match field("[#N] FIELD") {
            Ok((field, rest)) if rest.is_empty() => Action::SetChecked {
                field,
                checked: name == "check",
            },
            _ => return usage("[#N] FIELD"),
        },
        "pick" => match field("[#N] FIELD LINE") {
            Ok((field, line)) if !line.is_empty() => Action::Pick { field, line },
            _ => return usage("[#N] FIELD LINE"),
        },
        "press" if !rest.is_empty() => Action::Press {
            label: rest.to_owned(),
        },
        "press" => return usage("[#N] LABEL"),
        "show" if rest.is_empty() => return Ok(Input::Show(island)),
        "show" => return usage("[#N]"),
        "help" => return Ok(Input::Help),
        "quit" => return Ok(Input::Quit),
        _ => return Err(format!("unknown driver command /{name} (try /help)")),
    };
    Ok(Input::Act(island, action))
}

/// The first word of `s`, and the rest without leading whitespace.
fn split_word(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    match s.find(char::is_whitespace) {
        Some(i) => (&s[..i], s[i..].trim_start()),
        None => (s, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn act(island: Option<IslandId>, action: Action) -> Result<Input, String> {
        Ok(Input::Act(island, action))
    }

    #[test]
    fn parses_lines() {
        assert_eq!(parse("  "), Ok(Input::Nothing));
        assert_eq!(parse(" echo  a b "), Ok(Input::Run("echo  a b".into())));
        assert_eq!(
            parse("/type name Ada  Lovelace"),
            act(
                None,
                Action::Type {
                    field: "name".into(),
                    text: "Ada  Lovelace".into()
                }
            )
        );
        assert_eq!(
            parse("/type #3 name"),
            act(
                Some(3),
                Action::Type {
                    field: "name".into(),
                    text: "".into()
                }
            )
        );
        assert_eq!(
            parse("/uncheck sub"),
            act(
                None,
                Action::SetChecked {
                    field: "sub".into(),
                    checked: false
                }
            )
        );
        assert_eq!(
            parse("/pick fruit Apple pie"),
            act(
                None,
                Action::Pick {
                    field: "fruit".into(),
                    line: "Apple pie".into()
                }
            )
        );
        assert_eq!(
            parse("/press #2 Place order"),
            act(
                Some(2),
                Action::Press {
                    label: "Place order".into()
                }
            )
        );
        assert_eq!(parse("/show #4"), Ok(Input::Show(Some(4))));
        assert_eq!(parse("/quit"), Ok(Input::Quit));
    }

    #[test]
    fn rejects_bad_driver_commands() {
        assert_eq!(parse("/press"), Err("usage: /press [#N] LABEL".into()));
        assert_eq!(parse("/check a b"), Err("usage: /check [#N] FIELD".into()));
        assert_eq!(parse("/type #x a"), Err("bad island #x".into()));
        assert!(
            parse("/fly")
                .unwrap_err()
                .contains("unknown driver command")
        );
    }

    #[test]
    fn finds_changed_lines() {
        let old = "column\n  label \"a\"\n  progress 1%\n";
        let new = "column\n  label \"a\"\n  progress 2%\n";
        assert_eq!(changed_lines(old, new), Some(vec!["  progress 2%"]));
        assert_eq!(changed_lines(old, old), Some(vec![]));
        assert_eq!(changed_lines(old, "column\n"), None);
    }

    #[test]
    fn defaults_to_the_newest_live_island() {
        let island = || kettle_ui::IslandBuilder::new(kettle_ui::widget::column([])).freeze();
        let mut state = State::default();
        assert!(state.island(None).is_err());
        state.islands.insert(1, (1, island().unwrap(), true));
        state.islands.insert(2, (1, island().unwrap(), true));
        state.islands.insert(3, (2, island().unwrap(), false));
        assert_eq!(state.island(None), Ok(2));
        assert_eq!(state.island(Some(3)), Ok(3));
    }
}
