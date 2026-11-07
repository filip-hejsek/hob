//! The hob command interpreter, independent of any GUI toolkit.
//!
//! Commands are registered on an [`Interpreter`] by name, with a one-line
//! description. Running a command
//! line ([`Interpreter::run`]) starts the command on its own thread. The
//! command talks to the user through a [`Context`], in the style of a terminal
//! program: [`Context::print`] shows a message, [`Context::print_island`] shows
//! a frozen island (e.g. a result), and [`Context::show`] shows a kettle-ui
//! island and blocks until the user submits or cancels it. [`Context::error`],
//! [`warn`](Context::warn), [`info`](Context::info) and
//! [`debug`](Context::debug) log messages ([`LogRecord`]s), and
//! [`Context::log_records`] reads back all records the frontend has.
//! [`Context::clear`] clears the scrollback.
//!
//! [`builtins`] has commands that work with any frontend, such as a log viewer.
//!
//! A frontend (such as `hob-gui`) carries out these [`Request`]s. For its
//! prompt, there's a [`History`] of entered lines and
//! [completion](Interpreter::complete), and a [`Session`] to save and
//! restore across restarts.
//!
//! ```
//! use hob_core::Interpreter;
//!
//! let mut interpreter = Interpreter::new();
//! interpreter.register("hello", "Greet someone", |ctx, args| {
//!     ctx.print(format!("Hello, {}!", args.join(" ")));
//!     Ok(())
//! });
//! // A frontend handles the requests until the stream ends.
//! let requests = interpreter.run("hello world").unwrap();
//! ```

pub mod builtins;
mod history;
pub mod logger;
mod record;
mod session;

use std::collections::BTreeMap;
use std::fmt;
use std::panic::{self, AssertUnwindSafe};
use std::pin::Pin;
use std::sync::{Arc, mpsc as std_mpsc};
use std::task::{Context as TaskContext, Poll};
use std::thread;

use futures_channel::mpsc;
use futures_core::Stream;
use kettle_ui::{FrozenIsland, IslandBuilder, Outcome};

pub use history::History;
pub use record::{Level, LogRecord};
pub use session::{Block, Entry, Item, Session, SessionError};

/// What a command returns: any error is shown to the user.
pub type CommandResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

type CommandFn = Arc<dyn Fn(&mut Context, &[String]) -> CommandResult + Send + Sync>;

/// A registered command.
#[derive(Clone)]
struct Command {
    description: String,
    run: CommandFn,
}

/// The registered commands, by name. Shared with running commands, see
/// [`Context::commands`].
type Commands = Arc<BTreeMap<String, Command>>;

/// Runs command lines by looking up registered commands.
#[derive(Default)]
pub struct Interpreter {
    commands: Commands,
}

impl Interpreter {
    /// An interpreter with no commands.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a command. It gets the words after its name as arguments.
    /// The description says in one line what it does, e.g. "Print the
    /// arguments"; `help` lists it.
    ///
    /// # Panics
    ///
    /// If a command with this name is already registered, or the description
    /// is empty or has more than one line.
    pub fn register(
        &mut self,
        name: &str,
        description: impl Into<String>,
        run: impl Fn(&mut Context, &[String]) -> CommandResult + Send + Sync + 'static,
    ) {
        let description = description.into();
        assert!(
            !description.trim().is_empty() && !description.contains('\n'),
            "the description of {name:?} must be one non-empty line"
        );
        let command = Command {
            description,
            run: Arc::new(run),
        };
        let previous = Arc::make_mut(&mut self.commands).insert(name.to_owned(), command);
        assert!(previous.is_none(), "command {name:?} is already registered");
    }

    /// The names of all commands, sorted.
    pub fn commands(&self) -> impl Iterator<Item = &str> {
        self.commands.keys().map(String::as_str)
    }

    /// Completes the command name at the end of `line`, like a shell does on
    /// Tab: as far as it's unambiguous, with a space after a complete name.
    /// Arguments aren't completed yet.
    pub fn complete(&self, line: &str) -> Completion {
        let start = line.len() - line.trim_start().len();
        let (before, word) = line.split_at(start);
        let unchanged = Completion {
            line: line.to_owned(),
            candidates: Vec::new(),
        };
        if word.contains(char::is_whitespace) {
            return unchanged;
        }
        let matches: Vec<&str> = self.commands().filter(|c| c.starts_with(word)).collect();
        match matches.as_slice() {
            [] => unchanged,
            [only] => Completion {
                line: format!("{before}{only} "),
                candidates: Vec::new(),
            },
            [first, rest @ ..] => {
                let len = rest
                    .iter()
                    .fold(first.len(), |len, m| common_prefix_len(&first[..len], m));
                Completion {
                    line: format!("{before}{}", &first[..len]),
                    candidates: matches.iter().map(|&m| m.to_owned()).collect(),
                }
            }
        }
    }

    /// Starts running a command line on a new thread. The line is split into
    /// words at whitespace; the first is the command name.
    ///
    /// Returns the command's requests, which the frontend must carry out. The
    /// stream ends when the command finishes. If the command fails or panics,
    /// its last request is a [`Request::Log`] of the error.
    pub fn run(&self, line: &str) -> Result<Requests, Error> {
        let mut words = line.split_whitespace().map(str::to_owned);
        let name = words.next().ok_or(Error::Empty)?;
        let Some(command) = self.commands.get(&name).map(|c| c.run.clone()) else {
            return Err(Error::UnknownCommand {
                name,
                known: self.commands.keys().cloned().collect(),
            });
        };
        let args: Vec<String> = words.collect();
        let (sender, receiver) = mpsc::unbounded();
        let mut ctx = Context {
            name: name.clone(),
            commands: self.commands.clone(),
            sender,
        };
        thread::Builder::new()
            .name(format!("hob command {name}"))
            .spawn(move || {
                let error = match panic::catch_unwind(AssertUnwindSafe(|| command(&mut ctx, &args)))
                {
                    Ok(Ok(())) => return,
                    Ok(Err(e)) => e.to_string(),
                    Err(panic) => match panic_message(&*panic) {
                        Some(message) => format!("panicked: {message}"),
                        None => "panicked".to_owned(),
                    },
                };
                ctx.error(error);
            })
            .map_err(Error::Spawn)?;
        Ok(Requests(receiver))
    }
}

/// The message of a panic, if it has one (`panic!` with a message does).
fn panic_message(panic: &(dyn std::any::Any + Send)) -> Option<&str> {
    match panic.downcast_ref::<&str>() {
        Some(message) => Some(message),
        None => panic.downcast_ref::<String>().map(String::as_str),
    }
}

/// What [`Interpreter::complete`] made of a line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    /// The line, completed as far as it's unambiguous.
    pub line: String,
    /// The possible completions, if there are several; otherwise empty.
    pub candidates: Vec<String>,
}

/// The length in bytes of the longest common prefix of `a` and `b`.
fn common_prefix_len(a: &str, b: &str) -> usize {
    a.char_indices()
        .find(|&(i, c)| !b[i..].starts_with(c))
        .map_or(a.len(), |(i, _)| i)
}

/// Why [`Interpreter::run`] couldn't start a command.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The line has no command name.
    Empty,
    /// No command with this name is registered.
    UnknownCommand {
        /// The name in the command line.
        name: String,
        /// The names of all registered commands.
        known: Vec<String>,
    },
    /// The command's thread couldn't be started.
    Spawn(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Empty => f.write_str("no command given"),
            Error::UnknownCommand { name, known } => {
                write!(
                    f,
                    "unknown command {name:?} (commands: {})",
                    known.join(", ")
                )
            }
            Error::Spawn(e) => write!(f, "couldn't start the command: {e}"),
        }
    }
}

impl std::error::Error for Error {}

/// How a command talks to the user. Its methods block the command's thread,
/// not the frontend.
pub struct Context {
    /// The command's name, as the target of its log records.
    name: String,
    commands: Commands,
    sender: mpsc::UnboundedSender<Request>,
}

impl Context {
    /// All registered commands, sorted by name: their names and
    /// descriptions.
    pub fn commands(&self) -> impl Iterator<Item = (&str, &str)> {
        (self.commands.iter()).map(|(name, command)| (name.as_str(), command.description.as_str()))
    }

    /// Shows a message to the user.
    pub fn print(&self, message: impl Into<String>) {
        // If the frontend is gone, nobody would see it anyway.
        let _ = self.sender.unbounded_send(Request::Print(message.into()));
    }

    /// Logs an error: something failed. Returning an error from the command
    /// logs it too.
    pub fn error(&self, message: impl Into<String>) {
        self.log(Level::Error, message);
    }

    /// Logs a warning: something may be wrong.
    pub fn warn(&self, message: impl Into<String>) {
        self.log(Level::Warn, message);
    }

    /// Logs something useful to know.
    pub fn info(&self, message: impl Into<String>) {
        self.log(Level::Info, message);
    }

    /// Logs something only useful for debugging. It isn't saved with the
    /// session, and frontends may hide it.
    pub fn debug(&self, message: impl Into<String>) {
        self.log(Level::Debug, message);
    }

    fn log(&self, level: Level, message: impl Into<String>) {
        let record = LogRecord::new(level, &self.name, message);
        let _ = self.sender.unbounded_send(Request::Log(record));
    }

    /// All log records the frontend has, oldest first: from commands and from
    /// elsewhere, including hidden and ephemeral ones.
    pub fn log_records(&self) -> Result<Vec<LogRecord>, Closed> {
        let (reply, receiver) = std_mpsc::channel();
        let request = Request::LogRecords(RecordsReply(reply));
        self.sender.unbounded_send(request).map_err(|_| Closed)?;
        receiver.recv().map_err(|_| Closed)
    }

    /// Clears the scrollback, like `clear` in a terminal. Blocks of other
    /// commands that are still running are kept, and so are log records, for
    /// [`log_records`](Context::log_records). This command's own block is
    /// cleared too, so it should be the last thing the command does: the
    /// frontend may drop what it shows afterwards.
    pub fn clear(&self) {
        let _ = self.sender.unbounded_send(Request::Clear);
    }

    /// Shows a frozen island, e.g. to present a result. Create it with
    /// [`IslandBuilder::freeze`].
    pub fn print_island(&self, island: FrozenIsland) {
        let _ = self.sender.unbounded_send(Request::PrintIsland(island));
    }

    /// Shows an island and waits until the user submits or cancels it.
    pub fn show(&self, builder: IslandBuilder) -> Result<Outcome, ShowError> {
        let (reply, receiver) = std_mpsc::channel();
        let show = Show {
            builder,
            reply: Reply(reply),
        };
        self.sender
            .unbounded_send(Request::Show(show))
            .map_err(|_| ShowError::Closed)?;
        match receiver.recv() {
            Ok(Ok(outcome)) => Ok(outcome),
            Ok(Err(e)) => Err(ShowError::Invalid(e)),
            Err(_) => Err(ShowError::Closed),
        }
    }
}

/// Why [`Context::show`] failed.
#[derive(Debug)]
#[non_exhaustive]
pub enum ShowError {
    /// The island is invalid (see [`IslandBuilder::build`]).
    Invalid(kettle_ui::Error),
    /// The frontend closed the island without an outcome, e.g. because it is
    /// shutting down.
    Closed,
}

impl fmt::Display for ShowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ShowError::Invalid(e) => write!(f, "invalid island: {e}"),
            ShowError::Closed => f.write_str("the island was closed"),
        }
    }
}

impl std::error::Error for ShowError {}

/// The frontend dropped a request without replying, e.g. because it is
/// shutting down.
#[derive(Debug)]
pub struct Closed;

impl fmt::Display for Closed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the frontend closed the request")
    }
}

impl std::error::Error for Closed {}

/// Something a running command asks the frontend to do.
pub enum Request {
    /// Show a message to the user.
    Print(String),
    /// Show a log record to the user (and save it, unless it's ephemeral).
    Log(LogRecord),
    /// Show a frozen island to the user.
    PrintIsland(FrozenIsland),
    /// Show an island, and reply with its outcome.
    Show(Show),
    /// Reply with all log records; see [`Context::log_records`].
    LogRecords(RecordsReply),
    /// Clear the scrollback; see [`Context::clear`].
    Clear,
}

/// Sends log records back to the command; see [`Context::log_records`].
pub struct RecordsReply(std_mpsc::Sender<Vec<LogRecord>>);

impl RecordsReply {
    /// Sends the records, oldest first.
    pub fn send(self, records: Vec<LogRecord>) {
        let _ = self.0.send(records);
    }
}

/// A request to show an island; see [`Context::show`].
pub struct Show {
    builder: IslandBuilder,
    reply: Reply,
}

impl Show {
    /// The island to show, and where to send its outcome.
    pub fn into_parts(self) -> (IslandBuilder, Reply) {
        (self.builder, self.reply)
    }
}

/// Sends the outcome of a shown island back to the command. Dropping it
/// without sending makes [`Context::show`] fail with [`ShowError::Closed`].
pub struct Reply(std_mpsc::Sender<Result<Outcome, kettle_ui::Error>>);

impl Reply {
    /// Sends the outcome: the user submitted or cancelled the island.
    pub fn send(self, outcome: Outcome) {
        // The command may have panicked meanwhile; then nobody is waiting.
        let _ = self.0.send(Ok(outcome));
    }

    /// Reports that the island couldn't be shown because it's invalid.
    pub fn fail(self, error: kettle_ui::Error) {
        let _ = self.0.send(Err(error));
    }
}

/// The requests of one running command. The stream ends when the command
/// finishes.
pub struct Requests(mpsc::UnboundedReceiver<Request>);

impl Stream for Requests {
    type Item = Request;

    fn poll_next(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Request>> {
        Pin::new(&mut self.get_mut().0).poll_next(cx)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use kettle_ui::widget::column;

    use super::*;

    /// Waits for the next request; `None` once the command has finished.
    fn next(requests: &mut Requests) -> Option<Request> {
        loop {
            match requests.0.try_recv() {
                Ok(request) => return Some(request),
                Err(mpsc::TryRecvError::Closed) => return None,
                Err(mpsc::TryRecvError::Empty) => thread::sleep(Duration::from_millis(1)),
            }
        }
    }

    /// Waits for a log record; returns it as `level: message`.
    fn logged(requests: &mut Requests) -> String {
        match next(requests) {
            Some(Request::Log(record)) => record.to_string(),
            Some(_) => panic!("expected Log"),
            None => panic!("expected Log, but the command finished"),
        }
    }

    fn printed(requests: &mut Requests) -> String {
        match next(requests) {
            Some(Request::Print(message)) => message,
            Some(_) => panic!("expected Print"),
            None => panic!("expected Print, but the command finished"),
        }
    }

    fn interpreter() -> Interpreter {
        let mut i = Interpreter::new();
        i.register("echo", "Test", |ctx, args| {
            ctx.print(args.join("|"));
            Ok(())
        });
        i.register("fail", "Test", |_, _| Err("it broke".into()));
        i.register("panic", "Test", |_, _| panic!("test panic"));
        i.register("ask", "Test", |ctx, _| {
            match ctx.show(IslandBuilder::new(column([]))) {
                Ok(Outcome::Submitted(_)) => ctx.print("submitted"),
                Ok(Outcome::Cancelled) => ctx.print("cancelled"),
                Err(e) => ctx.print(e.to_string()),
            }
            Ok(())
        });
        i
    }

    #[test]
    fn arguments_are_split_at_whitespace() {
        let mut requests = interpreter().run("  echo a  b\tc ").unwrap();
        assert_eq!(printed(&mut requests), "a|b|c");
        assert!(next(&mut requests).is_none());
    }

    #[test]
    fn unknown_command_lists_known_ones() {
        let error = interpreter().run("nope").err().unwrap();
        assert_eq!(
            error.to_string(),
            r#"unknown command "nope" (commands: ask, echo, fail, panic)"#
        );
    }

    #[test]
    fn empty_line_is_an_error() {
        assert!(matches!(interpreter().run("  "), Err(Error::Empty)));
    }

    #[test]
    fn errors_and_panics_are_logged() {
        let mut requests = interpreter().run("fail").unwrap();
        assert_eq!(logged(&mut requests), "error: it broke");
        let mut requests = interpreter().run("panic").unwrap();
        assert_eq!(logged(&mut requests), "error: panicked: test panic");
    }

    #[test]
    fn commands_log_with_their_name_as_target() {
        let mut i = Interpreter::new();
        i.register("chatty", "Test", |ctx, _| {
            ctx.warn("careful");
            ctx.debug("details");
            Ok(())
        });
        let mut requests = i.run("chatty").unwrap();
        let Some(Request::Log(warning)) = next(&mut requests) else {
            panic!("expected Log");
        };
        assert_eq!((warning.level, &*warning.target), (Level::Warn, "chatty"));
        assert!(!warning.ephemeral);
        assert_eq!(logged(&mut requests), "debug: details");
    }

    #[test]
    fn frozen_islands_can_be_printed() {
        let mut i = Interpreter::new();
        i.register("result", "Test", |ctx, _| {
            ctx.print_island(IslandBuilder::new(column([])).freeze()?);
            Ok(())
        });
        let mut requests = i.run("result").unwrap();
        assert!(matches!(next(&mut requests), Some(Request::PrintIsland(_))));
        assert!(next(&mut requests).is_none());
    }

    #[test]
    fn show_waits_for_the_outcome() {
        let mut requests = interpreter().run("ask").unwrap();
        let Some(Request::Show(show)) = next(&mut requests) else {
            panic!("expected Show");
        };
        let (_, reply) = show.into_parts();
        reply.send(Outcome::Cancelled);
        assert_eq!(printed(&mut requests), "cancelled");
    }

    #[test]
    fn show_fails_when_the_reply_is_dropped() {
        let mut requests = interpreter().run("ask").unwrap();
        let Some(Request::Show(show)) = next(&mut requests) else {
            panic!("expected Show");
        };
        drop(show);
        assert_eq!(printed(&mut requests), "the island was closed");
    }

    fn completion(line: &str, candidates: &[&str]) -> Completion {
        Completion {
            line: line.to_owned(),
            candidates: candidates.iter().map(|&c| c.to_owned()).collect(),
        }
    }

    #[test]
    fn completes_command_names() {
        let mut i = Interpreter::new();
        for name in ["fruit", "fruit-search", "contact", "căt", "cătalog"] {
            i.register(name, "Test", |_, _| Ok(()));
        }
        assert_eq!(i.complete("co"), completion("contact ", &[]));
        assert_eq!(i.complete("  co"), completion("  contact ", &[]));
        assert_eq!(
            i.complete("fr"),
            completion("fruit", &["fruit", "fruit-search"])
        );
        assert_eq!(
            i.complete("c"),
            completion("c", &["contact", "căt", "cătalog"])
        );
        assert_eq!(i.complete("că"), completion("căt", &["căt", "cătalog"]));
        assert_eq!(i.complete("x"), completion("x", &[]));
        // Arguments aren't completed.
        assert_eq!(i.complete("contact co"), completion("contact co", &[]));
        assert_eq!(i.complete("contact "), completion("contact ", &[]));
    }

    #[test]
    fn completing_nothing_lists_all_commands() {
        let c = interpreter().complete("");
        assert_eq!(c, completion("", &["ask", "echo", "fail", "panic"]));
    }

    #[test]
    #[should_panic(expected = "must be one non-empty line")]
    fn descriptions_must_be_one_line() {
        Interpreter::new().register("x", "One\ntwo", |_, _| Ok(()));
    }

    #[test]
    #[should_panic(expected = "already registered")]
    fn registering_twice_panics() {
        let mut i = interpreter();
        i.register("echo", "Test", |_, _| Ok(()));
    }
}
