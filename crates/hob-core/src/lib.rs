//! The hob command interpreter, independent of any GUI toolkit.
//!
//! Commands are registered on an [`Interpreter`] by name. Running a command
//! line ([`Interpreter::run`]) starts the command on its own thread. The
//! command talks to the user through a [`Context`], in the style of a terminal
//! program: [`Context::print`] shows a message, [`Context::print_island`] shows
//! a frozen island (e.g. a result), and [`Context::show`] shows a kettle-ui
//! island and blocks until the user submits or cancels it.
//!
//! A frontend (such as `hob-gui`) carries out these [`Request`]s.
//!
//! ```
//! use hob_core::Interpreter;
//!
//! let mut interpreter = Interpreter::new();
//! interpreter.register("hello", |ctx, args| {
//!     ctx.print(format!("Hello, {}!", args.join(" ")));
//!     Ok(())
//! });
//! // A frontend handles the requests until the stream ends.
//! let requests = interpreter.run("hello world").unwrap();
//! ```

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

/// What a command returns: any error is shown to the user.
pub type CommandResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

type CommandFn = Arc<dyn Fn(&mut Context, &[String]) -> CommandResult + Send + Sync>;

/// Runs command lines by looking up registered commands.
#[derive(Default)]
pub struct Interpreter {
    commands: BTreeMap<String, CommandFn>,
}

impl Interpreter {
    /// An interpreter with no commands.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a command. It gets the words after its name as arguments.
    ///
    /// # Panics
    ///
    /// If a command with this name is already registered.
    pub fn register(
        &mut self,
        name: &str,
        run: impl Fn(&mut Context, &[String]) -> CommandResult + Send + Sync + 'static,
    ) {
        let previous = self.commands.insert(name.to_owned(), Arc::new(run));
        assert!(previous.is_none(), "command {name:?} is already registered");
    }

    /// The names of all commands, sorted.
    pub fn commands(&self) -> impl Iterator<Item = &str> {
        self.commands.keys().map(String::as_str)
    }

    /// Starts running a command line on a new thread. The line is split into
    /// words at whitespace; the first is the command name.
    ///
    /// Returns the command's requests, which the frontend must carry out. The
    /// stream ends when the command finishes. If the command fails or panics,
    /// its last request is a [`Request::Print`] of the error.
    pub fn run(&self, line: &str) -> Result<Requests, Error> {
        let mut words = line.split_whitespace().map(str::to_owned);
        let name = words.next().ok_or(Error::Empty)?;
        let Some(command) = self.commands.get(&name).cloned() else {
            return Err(Error::UnknownCommand {
                name,
                known: self.commands.keys().cloned().collect(),
            });
        };
        let args: Vec<String> = words.collect();
        let (sender, receiver) = mpsc::unbounded();
        let mut ctx = Context { sender };
        thread::Builder::new()
            .name(format!("hob command {name}"))
            .spawn(move || {
                let error = match panic::catch_unwind(AssertUnwindSafe(|| command(&mut ctx, &args)))
                {
                    Ok(Ok(())) => return,
                    Ok(Err(e)) => e.to_string(),
                    Err(_) => "panicked".to_owned(),
                };
                ctx.print(format!("{name}: {error}"));
            })
            .map_err(Error::Spawn)?;
        Ok(Requests(receiver))
    }
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
    sender: mpsc::UnboundedSender<Request>,
}

impl Context {
    /// Shows a message to the user.
    pub fn print(&self, message: impl Into<String>) {
        // If the frontend is gone, nobody would see it anyway.
        let _ = self.sender.unbounded_send(Request::Print(message.into()));
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

/// Something a running command asks the frontend to do.
pub enum Request {
    /// Show a message to the user.
    Print(String),
    /// Show a frozen island to the user.
    PrintIsland(FrozenIsland),
    /// Show an island, and reply with its outcome.
    Show(Show),
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

    fn printed(requests: &mut Requests) -> String {
        match next(requests) {
            Some(Request::Print(message)) => message,
            Some(_) => panic!("expected Print"),
            None => panic!("expected Print, but the command finished"),
        }
    }

    fn interpreter() -> Interpreter {
        let mut i = Interpreter::new();
        i.register("echo", |ctx, args| {
            ctx.print(args.join("|"));
            Ok(())
        });
        i.register("fail", |_, _| Err("it broke".into()));
        i.register("panic", |_, _| panic!("test panic"));
        i.register("ask", |ctx, _| {
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
    fn errors_and_panics_are_printed() {
        let mut requests = interpreter().run("fail").unwrap();
        assert_eq!(printed(&mut requests), "fail: it broke");
        let mut requests = interpreter().run("panic").unwrap();
        assert_eq!(printed(&mut requests), "panic: panicked");
    }

    #[test]
    fn frozen_islands_can_be_printed() {
        let mut i = Interpreter::new();
        i.register("result", |ctx, _| {
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

    #[test]
    #[should_panic(expected = "already registered")]
    fn registering_twice_panics() {
        let mut i = interpreter();
        i.register("echo", |_, _| Ok(()));
    }
}
