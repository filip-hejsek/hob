//! The hob command interpreter, independent of any GUI toolkit.
//!
//! Commands are registered on an [`Interpreter`] by name, with a one-line
//! description. A command can instead be registered by a prefix that starts
//! with a special character, such as `!`: it then gets the rest of the line
//! ([`Interpreter::register_prefix`]). Running a command
//! line ([`Interpreter::run`]) starts the command on its own thread. The
//! command talks to the user through a [`Context`], in the style of a terminal
//! program: [`Context::print`] shows a message, [`Context::print_island`] shows
//! a frozen island (e.g. a result), and [`Context::show`] shows a kettle-ui
//! island and blocks until the user submits or cancels it. [`Context::error`],
//! [`warn`](Context::warn), [`info`](Context::info) and
//! [`debug`](Context::debug) log messages ([`LogRecord`]s), and
//! [`Context::log_records`] reads back all records the frontend has.
//! [`Context::clear`] clears the scrollback, [`Context::exit`] asks the
//! frontend to exit, and [`Context::running_commands`] lists the other
//! commands still running.
//!
//! The interpreter has a working directory, simulated: the process's own
//! isn't changed. A command gets it with [`Context::cwd`], resolves paths
//! against it with [`Context::resolve`], and changes it with
//! [`Context::set_cwd`].
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
mod line;
pub mod logger;
mod record;
mod session;

use std::collections::BTreeMap;
use std::fmt;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError, mpsc as std_mpsc};
use std::task::{Context as TaskContext, Poll};
use std::thread;

use futures_channel::mpsc;
use futures_core::Stream;
use kettle_ui::{FrozenIsland, IslandBuilder, Outcome};

pub use history::History;
pub use line::trim_line_end;
pub use record::{Level, LogRecord};
pub use session::{Block, Entry, Item, Session, SessionError};

/// What a command returns: any error is shown to the user.
pub type CommandResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

/// A command's function, given its arguments.
type CommandFn<Args> = Arc<dyn Fn(&mut Context, &Args) -> CommandResult + Send + Sync>;

/// A command, with its arguments, ready to run on its thread.
type Job = Box<dyn FnOnce(&mut Context) -> CommandResult + Send>;

/// How a command is run.
#[derive(Clone)]
enum Run {
    /// With the words after its name.
    Words(CommandFn<[String]>),
    /// With the rest of the line after its prefix.
    Rest(CommandFn<str>),
}

/// A registered command.
#[derive(Clone)]
struct Command {
    description: String,
    run: Run,
}

/// The registered commands, by name (or prefix). Shared with running
/// commands, see [`Context::commands`].
type Commands = Arc<BTreeMap<String, Command>>;

/// The interpreter's working directory, shared with running commands so
/// that they can change it.
type Cwd = Arc<Mutex<PathBuf>>;

/// Runs command lines by looking up registered commands.
pub struct Interpreter {
    commands: Commands,
    cwd: Cwd,
}

impl Default for Interpreter {
    fn default() -> Self {
        Self::new()
    }
}

impl Interpreter {
    /// An interpreter with no commands. Its working directory is the
    /// process's (or `/`, if that can't be found).
    pub fn new() -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        Self {
            commands: Commands::default(),
            cwd: Arc::new(Mutex::new(cwd)),
        }
    }

    /// The working directory, which commands started from now on get.
    pub fn cwd(&self) -> PathBuf {
        lock(&self.cwd).clone()
    }

    /// Changes the working directory, for commands started from now on.
    ///
    /// # Panics
    ///
    /// If the path is relative.
    pub fn set_cwd(&self, path: impl Into<PathBuf>) {
        *lock(&self.cwd) = absolute(path.into());
    }

    /// Registers a command. It gets the words after its name as arguments.
    /// The description says in one line what it does, e.g. "Print the
    /// arguments"; `help` lists it.
    ///
    /// # Panics
    ///
    /// If a command with this name is already registered, the name doesn't
    /// start with a letter or digit (other first characters are reserved for
    /// other features), or the description is empty or has more than one line.
    pub fn register(
        &mut self,
        name: &str,
        description: impl Into<String>,
        run: impl Fn(&mut Context, &[String]) -> CommandResult + Send + Sync + 'static,
    ) {
        assert!(
            name.starts_with(char::is_alphanumeric),
            "the name {name:?} must start with a letter or digit"
        );
        self.insert(name, description.into(), Run::Words(Arc::new(run)));
    }

    /// Registers a command that's run for every line starting with `prefix`,
    /// e.g. `!` for `!ls -l`. It gets the rest of the line, unsplit (`ls -l`).
    /// If several prefixes match, the longest wins. `help` lists it like
    /// other commands; Tab doesn't complete it.
    ///
    /// # Panics
    ///
    /// If a command with this prefix is already registered, the prefix
    /// doesn't start with a special character (one that isn't a letter, digit
    /// or whitespace; quotes and `\` aren't allowed either, as they quote the
    /// command name) or contains whitespace, or the description is empty or
    /// has more than one line.
    pub fn register_prefix(
        &mut self,
        prefix: &str,
        description: impl Into<String>,
        run: impl Fn(&mut Context, &str) -> CommandResult + Send + Sync + 'static,
    ) {
        assert!(
            is_prefix(prefix)
                && !prefix.starts_with(['\'', '"', '\\'])
                && !prefix.contains(char::is_whitespace),
            "the prefix {prefix:?} must start with a special character other than a quote \
             or backslash, and have no whitespace"
        );
        self.insert(prefix, description.into(), Run::Rest(Arc::new(run)));
    }

    fn insert(&mut self, name: &str, description: String, run: Run) {
        assert!(
            !description.trim().is_empty() && !description.contains('\n'),
            "the description of {name:?} must be one non-empty line"
        );
        let command = Command { description, run };
        let previous = Arc::make_mut(&mut self.commands).insert(name.to_owned(), command);
        assert!(previous.is_none(), "command {name:?} is already registered");
    }

    /// The names (or prefixes) of all commands, sorted.
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
        if word.contains(char::is_whitespace) || is_prefix(word) {
            return unchanged;
        }
        let matches: Vec<&str> = self
            .commands()
            .filter(|c| !is_prefix(c) && c.starts_with(word))
            .collect();
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

    /// Starts running a command line on a new thread. If it starts with a
    /// special character (after any whitespace), it runs the command with the
    /// longest matching prefix. Otherwise it's split into words at whitespace,
    /// with quotes and backslashes as in a shell (`'a b'`, `"a b"`, `a\ b`)
    /// and `~` as the home directory; the first is the command name.
    ///
    /// Returns the command's requests, which the frontend must carry out. The
    /// stream ends when the command finishes. If the command fails or panics,
    /// its last request is a [`Request::Log`] of the error.
    pub fn run(&self, line: &str) -> Result<Requests, Error> {
        let line = line.trim_start();
        let unknown = |name: &str| Error::UnknownCommand {
            name: name.to_owned(),
            known: self.commands.keys().cloned().collect(),
        };
        let prefixed = (self.commands.iter())
            .filter(|(prefix, _)| is_prefix(prefix) && line.starts_with(prefix.as_str()))
            .max_by_key(|(prefix, _)| prefix.len());
        let (name, command): (String, Job) = if let Some((prefix, command)) = prefixed {
            let Run::Rest(run) = command.run.clone() else {
                unreachable!("prefixes are registered with Run::Rest");
            };
            let rest = line[prefix.len()..].to_owned();
            (prefix.clone(), Box::new(move |ctx| run(ctx, &rest)))
        } else {
            let words = line::split(line).map_err(|e| Error::Syntax(e.to_string()))?;
            let mut words = words.into_iter().map(|w| w.value);
            let name = words.next().ok_or(Error::Empty)?;
            // A quoted name can't run a prefix command.
            let command = (self.commands.get(&name))
                .filter(|_| !is_prefix(&name))
                .ok_or_else(|| unknown(&name))?;
            let Run::Words(run) = command.run.clone() else {
                unreachable!("names are registered with Run::Words");
            };
            let args: Vec<String> = words.collect();
            (name, Box::new(move |ctx| run(ctx, &args)))
        };
        let (sender, receiver) = mpsc::unbounded();
        let mut ctx = Context {
            name: name.clone(),
            commands: self.commands.clone(),
            cwd: self.cwd(),
            shared_cwd: self.cwd.clone(),
            sender,
        };
        thread::Builder::new()
            .name(format!("hob command {name}"))
            .spawn(move || {
                let error = match panic::catch_unwind(AssertUnwindSafe(|| command(&mut ctx))) {
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

/// Whether a command name is a prefix: it starts with a special character.
fn is_prefix(name: &str) -> bool {
    name.starts_with(|c: char| !c.is_alphanumeric() && !c.is_whitespace())
}

/// Locks the working directory. Nothing can panic while it's locked, but
/// a poisoned lock would still hold a valid path.
fn lock(cwd: &Cwd) -> std::sync::MutexGuard<'_, PathBuf> {
    cwd.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Checks that a new working directory is absolute.
fn absolute(path: PathBuf) -> PathBuf {
    assert!(
        path.is_absolute(),
        "the working directory must be absolute: {path:?}"
    );
    path
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
    /// The line couldn't be split into words, e.g. a quote isn't closed.
    Syntax(String),
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
            Error::Syntax(message) => f.write_str(message),
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
    /// The command's working directory.
    cwd: PathBuf,
    /// The interpreter's working directory.
    shared_cwd: Cwd,
    sender: mpsc::UnboundedSender<Request>,
}

impl Context {
    /// All registered commands, sorted by name: their names (or prefixes)
    /// and descriptions.
    pub fn commands(&self) -> impl Iterator<Item = (&str, &str)> {
        (self.commands.iter()).map(|(name, command)| (name.as_str(), command.description.as_str()))
    }

    /// The working directory: the interpreter's when the command started,
    /// unless the command changed it.
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Changes the working directory, like `cd`: for this command, and for
    /// commands started from now on. Commands already running keep theirs.
    /// The path isn't checked or normalized.
    ///
    /// # Panics
    ///
    /// If the path is relative; [`resolve`](Context::resolve) it first.
    pub fn set_cwd(&mut self, path: impl Into<PathBuf>) {
        self.cwd = absolute(path.into());
        *lock(&self.shared_cwd) = self.cwd.clone();
    }

    /// Turns a path given by the user into one the command can use: relative
    /// to the working directory. It isn't normalized, so `..` is left for the
    /// OS to follow. (`~` is expanded when the command line is split, so it
    /// isn't expanded here.)
    pub fn resolve(&self, path: impl AsRef<Path>) -> PathBuf {
        self.cwd.join(path)
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

    /// The command lines of the other commands still running, oldest first.
    pub fn running_commands(&self) -> Result<Vec<String>, Closed> {
        let (reply, receiver) = std_mpsc::channel();
        let request = Request::RunningCommands(LinesReply(reply));
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

    /// Asks the frontend to exit, like closing its window. Commands still
    /// running are interrupted, so this should be the last thing the command
    /// does. A frontend without a window of its own leaves it to its client.
    pub fn exit(&self) {
        let _ = self.sender.unbounded_send(Request::Exit);
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
    /// Reply with the command lines of the other running commands; see
    /// [`Context::running_commands`].
    RunningCommands(LinesReply),
    /// Clear the scrollback; see [`Context::clear`].
    Clear,
    /// Exit; see [`Context::exit`].
    Exit,
}

/// Sends log records back to the command; see [`Context::log_records`].
pub struct RecordsReply(std_mpsc::Sender<Vec<LogRecord>>);

impl RecordsReply {
    /// Sends the records, oldest first.
    pub fn send(self, records: Vec<LogRecord>) {
        let _ = self.0.send(records);
    }
}

/// Sends command lines back to the command; see
/// [`Context::running_commands`].
pub struct LinesReply(std_mpsc::Sender<Vec<String>>);

impl LinesReply {
    /// Sends the lines.
    pub fn send(self, lines: Vec<String>) {
        let _ = self.0.send(lines);
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
                Ok(Outcome::Pressed(id, _)) => ctx.print(format!("pressed {id}")),
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
    fn arguments_can_be_quoted() {
        let i = interpreter();
        assert_eq!(
            printed(&mut i.run(r#"'echo' "a b" c\ d"#).unwrap()),
            "a b|c d"
        );
        let Err(Error::Syntax(message)) = i.run("echo 'a") else {
            panic!("expected Syntax");
        };
        assert_eq!(message, "missing closing '");
    }

    #[test]
    fn quoted_names_are_not_prefixes() {
        let Err(Error::UnknownCommand { name, .. }) = prefixed().run("'!' x") else {
            panic!("expected UnknownCommand");
        };
        assert_eq!(name, "!");
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
    fn commands_share_the_working_directory() {
        let mut i = Interpreter::new();
        i.set_cwd("/start");
        i.register("cd", "Test", |ctx, args| {
            let path = ctx.resolve(&args[0]);
            ctx.set_cwd(path);
            ctx.print(ctx.cwd().display().to_string());
            Ok(())
        });
        i.register("pwd", "Test", |ctx, _| {
            ctx.print(ctx.cwd().display().to_string());
            Ok(())
        });
        let mut requests = i.run("cd sub").unwrap();
        assert_eq!(printed(&mut requests), "/start/sub");
        assert!(next(&mut requests).is_none());
        assert_eq!(i.cwd(), Path::new("/start/sub"));
        let mut requests = i.run("pwd").unwrap();
        assert_eq!(printed(&mut requests), "/start/sub");
    }

    #[test]
    fn running_commands_keep_their_working_directory() {
        let mut i = Interpreter::new();
        i.set_cwd("/old");
        i.register("wait", "Test", |ctx, _| {
            let _ = ctx.show(IslandBuilder::new(column([])));
            ctx.print(ctx.cwd().display().to_string());
            Ok(())
        });
        let mut requests = i.run("wait").unwrap();
        let Some(Request::Show(show)) = next(&mut requests) else {
            panic!("expected Show");
        };
        i.set_cwd("/new");
        drop(show);
        assert_eq!(printed(&mut requests), "/old");
    }

    #[test]
    fn paths_are_resolved_against_the_working_directory() {
        let mut i = Interpreter::new();
        i.set_cwd("/work");
        i.register("resolve", "Test", |ctx, args| {
            for arg in args {
                ctx.print(ctx.resolve(arg).display().to_string());
            }
            Ok(())
        });
        let mut requests = i.run("resolve a ../b /c ~x '~'").unwrap();
        for expected in ["/work/a", "/work/../b", "/c", "/work/~x", "/work/~"] {
            assert_eq!(printed(&mut requests), expected);
        }
        if let Some(home) = std::env::home_dir() {
            let mut requests = i.run("resolve ~ ~/d").unwrap();
            assert_eq!(printed(&mut requests), home.display().to_string());
            assert_eq!(printed(&mut requests), home.join("d").display().to_string());
        }
    }

    #[test]
    #[should_panic(expected = "must be absolute")]
    fn the_working_directory_must_be_absolute() {
        Interpreter::new().set_cwd("relative");
    }

    #[test]
    #[should_panic(expected = "must be one non-empty line")]
    fn descriptions_must_be_one_line() {
        Interpreter::new().register("x", "One\ntwo", |_, _| Ok(()));
    }

    #[test]
    #[should_panic(expected = "must start with a letter or digit")]
    fn names_must_not_start_with_a_special_character() {
        Interpreter::new().register("!x", "Test", |_, _| Ok(()));
    }

    #[test]
    #[should_panic(expected = "must start with a letter or digit")]
    fn names_must_not_be_empty() {
        Interpreter::new().register("", "Test", |_, _| Ok(()));
    }

    fn prefixed() -> Interpreter {
        let mut i = Interpreter::new();
        i.register("echo", "Test", |ctx, args| {
            ctx.print(args.join(" "));
            Ok(())
        });
        i.register_prefix("!", "Test", |ctx, rest| {
            ctx.print(format!("1:{rest}"));
            Ok(())
        });
        i.register_prefix("!!", "Test", |ctx, rest| {
            ctx.print(format!("2:{rest}"));
            Ok(())
        });
        i
    }

    #[test]
    fn prefix_commands_get_the_rest_of_the_line() {
        let i = prefixed();
        assert_eq!(printed(&mut i.run("!ls  -l").unwrap()), "1:ls  -l");
        assert_eq!(printed(&mut i.run("  ! ls").unwrap()), "1: ls");
        assert_eq!(printed(&mut i.run("!").unwrap()), "1:");
        assert_eq!(printed(&mut i.run("echo !x").unwrap()), "!x");
    }

    #[test]
    fn the_longest_prefix_wins() {
        let i = prefixed();
        assert_eq!(printed(&mut i.run("!!x").unwrap()), "2:x");
    }

    #[test]
    fn unmatched_prefixes_are_unknown_commands() {
        let Err(Error::UnknownCommand { name, .. }) = prefixed().run("@x y") else {
            panic!("expected UnknownCommand");
        };
        assert_eq!(name, "@x");
    }

    #[test]
    fn prefix_commands_are_not_completed() {
        let i = prefixed();
        assert_eq!(i.complete("").line, "echo ");
        assert_eq!(i.complete("!").line, "!");
    }

    #[test]
    #[should_panic(expected = "must start with a special character")]
    fn prefixes_must_start_with_a_special_character() {
        Interpreter::new().register_prefix("x", "Test", |_, _| Ok(()));
    }

    #[test]
    #[should_panic(expected = "must start with a special character")]
    fn prefixes_must_not_start_with_a_quote() {
        Interpreter::new().register_prefix("'", "Test", |_, _| Ok(()));
    }

    #[test]
    #[should_panic(expected = "must start with a special character")]
    fn prefixes_must_not_contain_whitespace() {
        Interpreter::new().register_prefix("! ", "Test", |_, _| Ok(()));
    }

    #[test]
    #[should_panic(expected = "already registered")]
    fn registering_twice_panics() {
        let mut i = interpreter();
        i.register("echo", "Test", |_, _| Ok(()));
    }
}
