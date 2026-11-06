//! A demo client for the headless hob frontend: an interactive command line.
//!
//! It starts the frontend (`hob --headless` from the same directory, or the
//! command given as arguments) and talks to it over JSON-RPC. Lines typed
//! are run as hob commands; lines starting with `/` act on islands (see
//! [`HELP`]). Everything the commands do is printed as it arrives, prefixed
//! with the command's block number; islands are printed as text outlines.

use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Write};
use std::process::{ChildStdin, Command, ExitCode, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use hob_core::Level;
use hob_headless::protocol::{self, BlockId, Call, ErrorObject, Incoming, IslandId, Notification};
use kettle_ui::FrozenIsland;
use serde_json::Value;

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
    let mut command = frontend_command()?;
    let mut child = (command.stdin(Stdio::piped()).stdout(Stdio::piped()))
        .spawn()
        .map_err(|e| format!("couldn't start {command:?}: {e}"))?;
    let mut frontend = child.stdin.take().expect("stdin is piped");
    let output = child.stdout.take().expect("stdout is piped");

    let state = Arc::new(Mutex::new(State::default()));
    let (responses, replies) = mpsc::channel();
    let reader_state = state.clone();
    let reader = thread::spawn(move || read_frontend(output, &reader_state, &responses));

    println!("{HELP}");
    let result = repl(&mut frontend, &state, &replies);
    // Ending its input makes the frontend exit.
    drop(frontend);
    let _ = child.wait();
    let _ = reader.join();
    result
}

/// The frontend command: from the arguments, or `hob --headless` next to
/// this program.
fn frontend_command() -> Result<Command, String> {
    let mut args = std::env::args_os().skip(1);
    if let Some(program) = args.next() {
        let mut command = Command::new(program);
        command.args(args);
        return Ok(command);
    }
    let exe = std::env::current_exe().map_err(|e| format!("can't find hob: {e}"))?;
    let mut command = Command::new(exe.with_file_name("hob"));
    command.arg("--headless");
    Ok(command)
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

fn repl(
    frontend: &mut ChildStdin,
    state: &Mutex<State>,
    replies: &Receiver<(Value, Result<Value, ErrorObject>)>,
) -> Result<(), String> {
    let mut next_id = 0;
    let mut lines = io::stdin().lock().lines();
    loop {
        print!("> ");
        let _ = io::stdout().flush();
        let Some(line) = lines.next() else {
            println!();
            return Ok(());
        };
        let line = line.map_err(|e| format!("couldn't read input: {e}"))?;
        let call = match parse(&line) {
            Ok(Input::Nothing) => continue,
            Ok(Input::Quit) => return Ok(()),
            Ok(Input::Help) => {
                println!("{HELP}");
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
                        None => println!("no island #{id}"),
                    },
                    Err(e) => println!("{e}"),
                }
                continue;
            }
            Ok(Input::Act(id, action)) => match state.lock().unwrap().island(id) {
                Ok(island) => action.call(island),
                Err(e) => {
                    println!("{e}");
                    continue;
                }
            },
            Err(e) => {
                println!("{e}");
                continue;
            }
        };
        next_id += 1;
        let request = protocol::request(next_id, &call);
        writeln!(frontend, "{request}").map_err(|e| format!("the frontend is gone: {e}"))?;
        // Notifications are printed meanwhile, by the reader thread.
        loop {
            let (id, result) = replies.recv().map_err(|_| "the frontend exited")?;
            if id == next_id {
                if let Err(error) = result {
                    println!("error: {}", error.message);
                }
                break;
            }
        }
    }
}

/// Prints notifications from the frontend, and passes replies on.
fn read_frontend(
    output: impl io::Read,
    state: &Mutex<State>,
    replies: &Sender<(Value, Result<Value, ErrorObject>)>,
) {
    let level = hob_core::logger::level_from_env();
    for line in BufReader::new(output).lines() {
        let Ok(line) = line else { return };
        match protocol::parse_incoming(&line) {
            Ok(Incoming::Response { id, result }) => {
                let _ = replies.send((id, result));
            }
            Ok(Incoming::Notification(n)) => print_notification(n, state, level),
            Err(e) => eprintln!("hob-cli: bad message from the frontend: {e}"),
        }
    }
}

fn print_notification(notification: Notification, state: &Mutex<State>, level: Level) {
    match notification {
        Notification::Printed { block, text } => {
            for line in text.lines() {
                println!("[{block}] {line}");
            }
        }
        Notification::Logged { block, record } => {
            if record.level <= level {
                let block = block.map_or("-".to_owned(), |b| b.to_string());
                println!("[{block}] {record}");
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
                Some(_) if !live => println!("[{block}] island #{island} closed"),
                Some((_, old, _)) => {
                    let old = kettle_ui_headless::outline(old);
                    let new = kettle_ui_headless::outline(&contents);
                    match changed_lines(&old, &new) {
                        Some(lines) => {
                            for line in lines {
                                println!("[{block}] island #{island}: {}", line.trim());
                            }
                        }
                        None => print_island(block, island, &contents, live),
                    }
                }
                None => print_island(block, island, &contents, live),
            }
            state.islands.insert(island, (block, contents, live));
        }
        Notification::Finished { block } => println!("[{block}] done"),
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
    println!("[{block}] island #{island}{kind}:");
    for line in kettle_ui_headless::outline(contents).lines() {
        println!("[{block}]   {line}");
    }
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
