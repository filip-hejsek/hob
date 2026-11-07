//! The hob shell: owns the main window, runs commands, and shows kettle-ui
//! islands in it.
//!
//! The shell itself is plain iced. It works like a terminal: a scrollback,
//! with a command prompt below it. Each command gets a block in the
//! scrollback: its command line, then what it prints, logs and the islands it
//! shows, in order. A live island freezes in place once the user submits or
//! cancels it. The `clear` command clears the scrollback, except for
//! commands still running, and `exit` closes the window.
//!
//! Log records that aren't from a command (from the shell itself, kettle-ui
//! or dependencies, through the `log` crate) are added to the scrollback as
//! notices; errors and warnings among them are shown, except warnings from
//! dependencies, which are rarely something the user can act on. Repeats of
//! a dependency's record are dropped. `HOB_LOG` sets the
//! lowest level shown (`info` by default; `debug` shows debug records).
//! Commands can read all records with
//! [`Context::log_records`](hob_core::Context::log_records).
//!
//! The prompt works like a shell's: Up and Down browse the history, and Tab
//! completes command names. Tab doesn't move focus. Anywhere, PageUp and
//! PageDown scroll the scrollback, and Ctrl+L clears it (like `clear`). Ctrl+D
//! in the empty prompt exits (like `exit`), unless commands are running: then
//! it says so, and focuses an island waiting for input. The bindings are a table (see the
//! `keys` module), to make them configurable later.
//!
//! A newly shown island gets focus, unless the user is busy elsewhere
//! (something other than the empty prompt has focus). When an island
//! freezes and nothing has focus, the prompt gets it back. Islands handle
//! some keys themselves (see [`kettle_ui_iced`]).
//!
//! The history and scrollback are saved as a [`Session`] after every change
//! and on exit, and restored on the next start. Commands still running on
//! exit are marked as interrupted. A command line starting with a space is
//! private: neither it nor its output is saved. It stays in the history
//! while its block is in the scrollback.

use std::fmt;
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hob_core::{History, Interpreter, Level, LogRecord, Reply, Request, Session, SessionError};
use iced::advanced::widget::operation::{Focusable, Outcome as OperationOutcome, Scrollable};
use iced::advanced::widget::{Operation, operate};
use iced::keyboard::{self, Modifiers};
use iced::widget::scrollable::AbsoluteOffset;
use iced::widget::{Id, column, container, operation, rule, scrollable, text, text_input};
use iced::{
    Border, Color, Element, Event, Font, Length, Subscription, Task, Theme, event, font, padding,
    window,
};
use kettle_ui::FrozenIsland;
use kettle_ui_iced::{Island, spacing, view_frozen};

use keys::{Action, Binding};

mod keys;

/// The target of the shell's own log records.
const TARGET: &str = "hob_gui";

/// Opens the main window, running commands with `interpreter`, and blocks
/// until it closes. If `session` is given, the session is restored from that
/// file, and saved to it. The prompt shows `placeholder` while it's empty,
/// e.g. a hint on how to list the commands.
pub fn run(
    interpreter: Interpreter,
    session: Option<PathBuf>,
    placeholder: impl Into<String>,
) -> iced::Result {
    let level = hob_core::logger::level_from_env();
    let records = hob_core::logger::install(level);
    application(
        Arc::new(interpreter),
        session,
        placeholder.into(),
        level,
        records,
    )
    .run()
}

/// The shell as an iced application, ready to run. Log records that aren't
/// from a command are read from `records`, if given.
fn application(
    interpreter: Arc<Interpreter>,
    session: Option<PathBuf>,
    placeholder: String,
    level: Level,
    records: Option<futures_channel::mpsc::UnboundedReceiver<LogRecord>>,
) -> iced::Application<impl iced::Program<State = Shell, Message = Message, Theme = iced::Theme>> {
    // The boot function is `Fn`, but it's only called once.
    let records = Mutex::new(records);
    iced::application(
        move || {
            let records = records.lock().unwrap().take();
            Shell::new(
                interpreter.clone(),
                session.clone(),
                placeholder.clone(),
                level,
                records,
            )
        },
        Shell::update,
        Shell::view,
    )
    .subscription(Shell::subscription)
    // Save the session before exiting.
    .exit_on_close_request(false)
    .title("hob")
    .window_size((640.0, 560.0))
}

const PROMPT: &str = "prompt";
const SCROLLBACK: &str = "scrollback";

struct Shell {
    interpreter: Arc<Interpreter>,
    /// Shown in the empty prompt.
    placeholder: String,
    /// Where the session is saved, if anywhere.
    session: Option<PathBuf>,
    /// The last error saving the session, so it's reported only once.
    save_error: Option<String>,
    /// The lowest level of log records shown.
    level: Level,
    prompt: String,
    history: History,
    /// Completions offered after Tab, shown below the prompt.
    candidates: Vec<String>,
    /// The key bindings.
    bindings: Vec<Binding>,
    /// The scrollback, oldest first.
    scrollback: Vec<Item>,
    /// Log records of cleared items, kept for `messages`, but not saved.
    cleared_records: Vec<LogRecord>,
    /// For block and island ids.
    next_id: u64,
}

type BlockId = u64;
type IslandId = u64;

/// Something in the scrollback.
enum Item {
    Block(Block),
    /// A log record that isn't from a command.
    Notice(LogRecord),
}

/// A command line and its output.
struct Block {
    id: BlockId,
    line: String,
    entries: Vec<Entry>,
    /// Whether the command is still running.
    running: bool,
    /// Whether the command was interrupted by exiting, in an earlier session.
    interrupted: bool,
}

/// Something a command showed.
enum Entry {
    Text(String),
    Log(LogRecord),
    Live {
        id: IslandId,
        island: Island,
        reply: Reply,
    },
    Frozen(FrozenIsland),
    /// A saved island that couldn't be restored. It's saved back unchanged.
    Unreadable {
        error: String,
        raw: serde_json::Value,
    },
}

#[derive(Clone, Debug)]
enum Message {
    PromptChanged(String),
    PromptSubmitted,
    /// A key was pressed (the shell's bindings decide what it does).
    Key(keyboard::Key, Modifiers),
    /// Performs a bound action; for one that needs the prompt, it has
    /// focus.
    Action(Action),
    /// Moves focus, depending on what has it now (the Id).
    Refocus(Refocus, Option<Id>),
    /// A request from the command of a block.
    Request(BlockId, Pending<Request>),
    Island(IslandId, kettle_ui_iced::Message),
    /// The command of a block finished.
    Finished(BlockId),
    /// A record from the `log` crate.
    Log(LogRecord),
    CloseRequested,
}

/// Where focus may go, see [`Message::Refocus`].
#[derive(Clone, Copy, Debug)]
enum Refocus {
    /// A new island, unless something other than the empty prompt has
    /// focus.
    Island(IslandId),
    /// The prompt, if nothing has focus.
    Prompt,
}

/// Makes a value that isn't `Clone` or `Debug` fit in a [`Message`]; it can
/// be taken out once.
struct Pending<T>(Arc<Mutex<Option<T>>>);

impl<T> Pending<T> {
    fn new(value: T) -> Self {
        Pending(Arc::new(Mutex::new(Some(value))))
    }

    fn take(&self) -> Option<T> {
        self.0.lock().unwrap().take()
    }
}

impl<T> Clone for Pending<T> {
    fn clone(&self) -> Self {
        Pending(self.0.clone())
    }
}

impl<T> fmt::Debug for Pending<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Pending(..)")
    }
}

impl Shell {
    fn new(
        interpreter: Arc<Interpreter>,
        session: Option<PathBuf>,
        placeholder: String,
        level: Level,
        records: Option<futures_channel::mpsc::UnboundedReceiver<LogRecord>>,
    ) -> (Self, Task<Message>) {
        let mut shell = Shell {
            placeholder,
            interpreter,
            session,
            save_error: None,
            level,
            prompt: String::new(),
            history: History::new(),
            candidates: Vec::new(),
            bindings: keys::defaults(),
            scrollback: Vec::new(),
            cleared_records: Vec::new(),
            next_id: 0,
        };
        shell.restore();
        let mut tasks = vec![operation::focus(Id::new(PROMPT))];
        if let Some(records) = records {
            tasks.push(Task::run(records, Message::Log));
        }
        (shell, Task::batch(tasks))
    }

    /// Restores the saved session, if there is one. Errors are reported; a
    /// file with bad contents is moved aside, so it isn't overwritten.
    fn restore(&mut self) {
        let Some(path) = self.session.clone() else {
            return;
        };
        let restored = match Session::load(&path) {
            Ok(Some(session)) => session,
            Ok(None) => return,
            Err(e) => {
                let mut message =
                    format!("Couldn't restore the session from {}: {e}", path.display());
                // The file was read, but its contents are no good.
                if !matches!(e, SessionError::Io(_)) {
                    match move_aside(&path) {
                        Ok(aside) => message += &format!("; moved it to {}", aside.display()),
                        Err(e) => message += &format!("; couldn't move it aside: {e}"),
                    }
                }
                eprintln!("hob: {message}");
                self.scrollback
                    .push(Item::Notice(LogRecord::new(Level::Error, TARGET, message)));
                return;
            }
        };
        self.history = History::from(restored.history);
        for item in restored.scrollback {
            let item = match item {
                hob_core::Item::Block(block) => Item::Block(Block {
                    id: self.next_id(),
                    line: block.line,
                    entries: block.entries.into_iter().map(Entry::restore).collect(),
                    running: false,
                    interrupted: block.interrupted,
                }),
                hob_core::Item::Notice(record) => Item::Notice(record),
            };
            self.scrollback.push(item);
        }
    }

    /// Saves the history and scrollback, with live islands frozen as they are
    /// now, and without ephemeral log records or private blocks.
    ///
    /// An error is shown once, as an ephemeral notice: it can't be saved,
    /// and saving it mustn't cause another save.
    fn save(&mut self) {
        let Some(path) = &self.session else {
            return;
        };
        let session = Session {
            history: self.history.saved_lines(),
            scrollback: (self.scrollback.iter())
                .filter(|item| !matches!(item, Item::Block(b) if History::is_private(&b.line)))
                .map(Item::to_saved)
                .collect(),
        };
        let notice = match session.save(path) {
            Ok(()) => match self.save_error.take() {
                Some(_) => LogRecord::new(Level::Info, TARGET, "Saving the session works again."),
                None => return,
            },
            Err(e) => {
                let message = format!("Couldn't save the session to {}: {e}", path.display());
                if self.save_error.as_ref() == Some(&message) {
                    return;
                }
                eprintln!("hob: {message}");
                self.save_error = Some(message.clone());
                LogRecord::new(Level::Error, TARGET, message).ephemeral()
            }
        };
        self.scrollback.push(Item::Notice(notice));
    }

    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::PromptChanged(prompt) => {
                self.prompt = prompt;
                self.history.stop_browsing();
                self.candidates.clear();
                Task::none()
            }
            Message::PromptSubmitted => {
                let task = self.run_prompt();
                self.save();
                task
            }
            Message::Key(key, modifiers) => match keys::lookup(&self.bindings, key, modifiers) {
                Some(action) if action.needs_prompt() => operation::is_focused(Id::new(PROMPT))
                    .then(move |focused| {
                        if focused {
                            Task::done(Message::Action(action))
                        } else {
                            Task::none()
                        }
                    }),
                Some(action) => self.perform(action),
                None => Task::none(),
            },
            Message::Action(action) => self.perform(action),
            Message::Refocus(target, focused) => self.refocus(target, focused),
            Message::Request(block, request) => match request.take() {
                Some(request) => {
                    let ephemeral = match &request {
                        Request::Log(record) => record.ephemeral,
                        // Changes nothing.
                        Request::LogRecords(_) => true,
                        _ => false,
                    };
                    let task = self.handle(block, request);
                    if !ephemeral {
                        self.save();
                    }
                    task
                }
                None => Task::none(),
            },
            Message::Island(id, message) => self.update_island(id, message),
            Message::Finished(id) => {
                if let Some(block) = self.block_mut(id) {
                    block.running = false;
                }
                self.save();
                Task::none()
            }
            Message::Log(record) => {
                let ephemeral = record.ephemeral;
                self.scrollback.push(Item::Notice(record));
                if !ephemeral {
                    self.save();
                }
                Task::none()
            }
            Message::CloseRequested => self.exit(),
        }
    }

    fn block_mut(&mut self, id: BlockId) -> Option<&mut Block> {
        self.scrollback.iter_mut().find_map(|item| match item {
            Item::Block(block) if block.id == id => Some(block),
            _ => None,
        })
    }

    fn run_prompt(&mut self) -> Task<Message> {
        // Trailing spaces are invisible (completion leaves one), so they
        // shouldn't reach the command or the history.
        let line = mem::take(&mut self.prompt).trim_end().to_owned();
        if line.is_empty() {
            return Task::none();
        }
        self.history.push(&line);
        self.candidates.clear();
        let id = self.next_id();
        let mut block = Block {
            id,
            line,
            entries: Vec::new(),
            running: false,
            interrupted: false,
        };
        let task = match self.interpreter.run(&block.line) {
            Ok(requests) => {
                block.running = true;
                // The stream ends when the command finishes.
                Task::run(requests, move |r| Message::Request(id, Pending::new(r)))
                    .chain(Task::done(Message::Finished(id)))
            }
            Err(e) => {
                let record = LogRecord::new(Level::Error, TARGET, e.to_string());
                block.entries.push(Entry::Log(record));
                Task::none()
            }
        };
        self.scrollback.push(Item::Block(block));
        task
    }

    fn perform(&mut self, action: Action) -> Task<Message> {
        let line = match action {
            Action::HistoryBack => self.history.back(&self.prompt).map(str::to_owned),
            Action::HistoryForward => self.history.forward().map(str::to_owned),
            Action::Complete => {
                let completion = self.interpreter.complete(&self.prompt);
                self.candidates = completion.candidates;
                self.history.stop_browsing();
                Some(completion.line)
            }
            Action::PageUp => return scroll_page(1.0),
            Action::PageDown => return scroll_page(-1.0),
            Action::Clear => {
                self.clear(None);
                self.save();
                return Task::none();
            }
            Action::Exit if self.prompt.is_empty() => return self.exit_if_idle(),
            Action::Exit => return Task::none(),
        };
        match line {
            Some(line) => {
                self.prompt = line;
                operation::move_cursor_to_end(Id::new(PROMPT))
            }
            None => Task::none(),
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        let close = window::close_requests().map(|_| Message::CloseRequested);
        let keys = event::listen_with(|event, _status, _window| match event {
            Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                Some(Message::Key(key, modifiers))
            }
            _ => None,
        });
        Subscription::batch([close, keys])
    }

    fn handle(&mut self, block: BlockId, request: Request) -> Task<Message> {
        let request = match request {
            Request::LogRecords(reply) => {
                reply.send(self.log_records());
                return Task::none();
            }
            Request::RunningCommands(reply) => {
                let others = self.running().filter(|b| b.id != block);
                reply.send(others.map(|b| b.line.clone()).collect());
                return Task::none();
            }
            Request::Clear => {
                self.clear(Some(block));
                return Task::none();
            }
            Request::Exit => {
                // The caller is done, so it isn't interrupted.
                if let Some(block) = self.block_mut(block) {
                    block.running = false;
                }
                return self.exit();
            }
            request => request,
        };
        let island_id = self.next_id();
        let Some(block) = self.block_mut(block) else {
            return Task::none();
        };
        let entry = match request {
            Request::Print(message) => {
                // Also on stdout, e.g. for scripts.
                println!("{message}");
                Entry::Text(message)
            }
            Request::Log(record) => {
                if record.level <= Level::Warn {
                    eprintln!("hob: {}: {record}", record.target);
                }
                Entry::Log(record)
            }
            Request::PrintIsland(island) => Entry::Frozen(island),
            Request::LogRecords(_)
            | Request::RunningCommands(_)
            | Request::Clear
            | Request::Exit => {
                unreachable!("handled above")
            }
            Request::Show(show) => {
                let (builder, reply) = show.into_parts();
                match Island::new(builder) {
                    Ok((island, task)) => {
                        block.entries.push(Entry::Live {
                            id: island_id,
                            island,
                            reply,
                        });
                        let focus = focused()
                            .map(move |id| Message::Refocus(Refocus::Island(island_id), id));
                        return Task::batch([
                            task.map(move |m| Message::Island(island_id, m)),
                            focus,
                        ]);
                    }
                    Err(e) => {
                        reply.fail(e);
                        return Task::none();
                    }
                }
            }
        };
        block.entries.push(entry);
        Task::none()
    }

    /// The blocks of commands still running, oldest first.
    fn running(&self) -> impl Iterator<Item = &Block> {
        self.scrollback.iter().filter_map(|item| match item {
            Item::Block(block) if block.running => Some(block),
            _ => None,
        })
    }

    /// Exits, unless commands are running: then says so instead, and moves
    /// focus to the newest island waiting for input, if any.
    fn exit_if_idle(&mut self) -> Task<Message> {
        let running: Vec<&Block> = self.running().collect();
        let waiting = (running.iter().rev()).find_map(|block| {
            block.entries.iter().rev().find_map(|e| match e {
                Entry::Live { island, .. } => Some((block, island)),
                _ => None,
            })
        });
        let (named, focus) = match (waiting, running.last()) {
            (Some((block, island)), _) => (block, island.focus()),
            (None, Some(block)) => (block, Task::none()),
            (None, None) => return self.exit(),
        };
        let message = match running.len() - 1 {
            0 => format!("Not exiting: {} is running", named.line),
            1 => format!(
                "Not exiting: {} and 1 other command are running",
                named.line
            ),
            n => format!(
                "Not exiting: {} and {n} other commands are running",
                named.line
            ),
        };
        let record = LogRecord::new(Level::Warn, TARGET, message).ephemeral();
        self.scrollback.push(Item::Notice(record));
        focus
    }

    /// Saves the session and exits.
    fn exit(&mut self) -> Task<Message> {
        self.save();
        iced::exit()
    }

    /// Clears the scrollback, except for blocks of commands still running
    /// other than `caller`'s. Their log records are kept for `messages`.
    /// Private lines of cleared blocks are forgotten from the history.
    fn clear(&mut self, caller: Option<BlockId>) {
        let (kept, cleared): (Vec<_>, Vec<_>) = mem::take(&mut self.scrollback)
            .into_iter()
            .partition(|item| matches!(item, Item::Block(b) if b.running && Some(b.id) != caller));
        self.scrollback = kept;
        self.cleared_records.extend(log_records(&cleared));
        let shown: Vec<&str> = (self.scrollback.iter())
            .filter_map(|item| match item {
                Item::Block(block) => Some(block.line.as_str()),
                Item::Notice(_) => None,
            })
            .collect();
        self.history.retain_private(|line| shown.contains(&line));
    }

    /// All log records, oldest first: of cleared items, then of the
    /// scrollback.
    fn log_records(&self) -> Vec<LogRecord> {
        let mut records = self.cleared_records.clone();
        records.extend(log_records(&self.scrollback));
        records
    }

    fn update_island(&mut self, id: IslandId, message: kettle_ui_iced::Message) -> Task<Message> {
        let entry = (self.scrollback.iter_mut())
            .flat_map(|item| match item {
                Item::Block(block) => &mut block.entries[..],
                Item::Notice(_) => &mut [],
            })
            .find(|e| matches!(e, Entry::Live { id: i, .. } if *i == id));
        // Messages can still arrive for an island that was just frozen.
        let Some(entry) = entry else {
            return Task::none();
        };
        let Entry::Live { island, .. } = &mut *entry else {
            unreachable!("found a live island");
        };
        let (task, outcome) = island.update(message);
        if let Some(outcome) = outcome {
            let frozen = Entry::Frozen(island.freeze());
            if let Entry::Live { reply, .. } = mem::replace(entry, frozen) {
                reply.send(outcome);
            }
            self.save();
            let focus = focused().map(|id| Message::Refocus(Refocus::Prompt, id));
            return Task::batch([task.map(move |m| Message::Island(id, m)), focus]);
        }
        task.map(move |m| Message::Island(id, m))
    }

    /// Moves focus to `target`, depending on what has focus now.
    fn refocus(&self, target: Refocus, focused: Option<Id>) -> Task<Message> {
        let prompt = Id::new(PROMPT);
        match target {
            Refocus::Island(id) => {
                let free = match &focused {
                    None => true,
                    Some(f) => *f == prompt && self.prompt.is_empty(),
                };
                match self.live_island(id) {
                    Some(island) if free => island.focus(),
                    _ => Task::none(),
                }
            }
            Refocus::Prompt if focused.is_none() => operation::focus(prompt),
            Refocus::Prompt => Task::none(),
        }
    }

    fn live_island(&self, id: IslandId) -> Option<&Island> {
        (self.scrollback.iter())
            .flat_map(|item| match item {
                Item::Block(block) => &block.entries[..],
                Item::Notice(_) => &[],
            })
            .find_map(|e| match e {
                Entry::Live { id: i, island, .. } if *i == id => Some(island),
                _ => None,
            })
    }

    fn view(&self) -> Element<'_, Message> {
        let items = self.scrollback.iter().filter_map(|item| match item {
            Item::Block(block) => Some(view_block(block, self.level)),
            Item::Notice(record) if shows_notice(record, self.level) => Some(view_record(record)),
            // Kept for `messages`.
            Item::Notice(_) => None,
        });
        let scrollback = column(items).spacing(2.0 * spacing());

        let prompt = text_input(&self.placeholder, &self.prompt)
            .id(Id::new(PROMPT))
            .on_input(Message::PromptChanged)
            .on_submit(Message::PromptSubmitted)
            // Like the command lines in the scrollback.
            .icon(text_input::Icon {
                font: BOLD,
                code_point: '>',
                size: None,
                spacing: 4.0,
                side: text_input::Side::Left,
            })
            .padding(padding::vertical(5.0))
            .style(prompt_style);

        column([
            scrollable(container(scrollback).padding(2.0 * spacing()))
                .id(Id::new(SCROLLBACK))
                .anchor_bottom()
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
            rule::horizontal(1).into(),
            container(
                // The prompt stays the first child, so it keeps its state
                // (focus) when the candidates appear.
                column([prompt.into()].into_iter().chain(
                    (!self.candidates.is_empty()).then(|| text(self.candidates.join("   ")).into()),
                ))
                .spacing(spacing()),
            )
            .padding(padding::vertical(spacing()).horizontal(2.0 * spacing()))
            .into(),
        ])
        .into()
    }
}

impl Item {
    /// The item as saved; live islands are frozen as they are now.
    fn to_saved(&self) -> hob_core::Item {
        match self {
            Item::Block(block) => hob_core::Item::Block(hob_core::Block {
                line: block.line.clone(),
                entries: block.entries.iter().map(Entry::to_saved).collect(),
                interrupted: block.running || block.interrupted,
            }),
            Item::Notice(record) => hob_core::Item::Notice(record.clone()),
        }
    }
}

impl Entry {
    fn restore(entry: hob_core::Entry) -> Self {
        match entry {
            hob_core::Entry::Text(text) => Entry::Text(text),
            hob_core::Entry::Log(record) => Entry::Log(record),
            hob_core::Entry::Island(island) => Entry::Frozen(island),
            hob_core::Entry::Unreadable { error, raw } => Entry::Unreadable { error, raw },
        }
    }

    fn to_saved(&self) -> hob_core::Entry {
        match self {
            Entry::Text(text) => hob_core::Entry::Text(text.clone()),
            Entry::Log(record) => hob_core::Entry::Log(record.clone()),
            Entry::Live { island, .. } => hob_core::Entry::Island(island.freeze()),
            Entry::Frozen(island) => hob_core::Entry::Island(island.clone()),
            Entry::Unreadable { error, raw } => hob_core::Entry::Unreadable {
                error: error.clone(),
                raw: raw.clone(),
            },
        }
    }
}

/// The log records in `items`, oldest first.
fn log_records(items: &[Item]) -> Vec<LogRecord> {
    (items.iter())
        .flat_map(|item| match item {
            Item::Block(block) => (block.entries.iter())
                .filter_map(|entry| match entry {
                    Entry::Log(record) => Some(record.clone()),
                    _ => None,
                })
                .collect(),
            Item::Notice(record) => vec![record.clone()],
        })
        .collect()
}

/// Finds out which widget has focus, if any (its Id; `None` also if it has
/// none). Unlike iced's `find_focused`, it always gives an answer.
fn focused() -> Task<Option<Id>> {
    struct Focused(Option<Id>);

    impl Operation<Option<Id>> for Focused {
        fn focusable(&mut self, id: Option<&Id>, _: iced::Rectangle, state: &mut dyn Focusable) {
            if state.is_focused() {
                self.0 = id.cloned();
            }
        }

        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<Option<Id>>)) {
            operate(self);
        }

        fn finish(&self) -> OperationOutcome<Option<Id>> {
            OperationOutcome::Some(self.0.clone())
        }
    }

    operate(Focused(None))
}

/// Scrolls the scrollback up by `pages` pages (down if negative).
fn scroll_page<T: Send + 'static>(pages: f32) -> Task<T> {
    struct ScrollPage(f32);

    impl<T> Operation<T> for ScrollPage {
        fn scrollable(
            &mut self,
            id: Option<&Id>,
            bounds: iced::Rectangle,
            content_bounds: iced::Rectangle,
            _: iced::Vector,
            state: &mut dyn Scrollable,
        ) {
            if id == Some(&Id::new(SCROLLBACK)) {
                // A bit less than a page, so some lines stay in view. The
                // scrollback is anchored at the bottom, so its offset grows
                // upwards.
                let y = self.0 * (bounds.height - 3.0 * spacing()).max(spacing());
                state.scroll_by(AbsoluteOffset { x: 0.0, y }, bounds, content_bounds);
            }
        }

        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<T>)) {
            operate(self);
        }
    }

    operate(ScrollPage(pages))
}

/// Moves a file to `<path>.bad`, and returns the new path.
fn move_aside(path: &Path) -> std::io::Result<PathBuf> {
    let mut aside = path.as_os_str().to_owned();
    aside.push(".bad");
    let aside = PathBuf::from(aside);
    std::fs::rename(path, &aside)?;
    Ok(aside)
}

/// Whether a notice is shown in the scrollback: errors, and warnings from our
/// crates, at most down to `level`.
fn shows_notice(record: &LogRecord, level: Level) -> bool {
    let lowest = match hob_core::logger::is_ours(&record.target) {
        true => Level::Warn,
        false => Level::Error,
    };
    record.level <= lowest.min(level)
}

/// Shown in a private block.
const PRIVATE_NOTE: &str = "Not saved: the command line starts with a space.";

/// The font of command lines, and of the prompt's `>`.
const BOLD: Font = Font {
    weight: font::Weight::Bold,
    ..Font::DEFAULT
};

/// The prompt has no box; its `>` is coloured while it's focused.
fn prompt_style(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let palette = theme.extended_palette();
    let icon = match status {
        text_input::Status::Focused { .. } => palette.primary.base.color,
        _ => palette.background.strong.color,
    };
    text_input::Style {
        background: Color::TRANSPARENT.into(),
        border: Border::default(),
        icon,
        ..text_input::default(theme, status)
    }
}

/// Shows a block; log records below `level` are hidden.
fn view_block(block: &Block, level: Level) -> Element<'_, Message> {
    let interrupted = block.interrupted.then(|| {
        text("Interrupted: hob exited while this command was running.")
            .style(text::secondary)
            .into()
    });
    let private =
        History::is_private(&block.line).then(|| text(PRIVATE_NOTE).style(text::secondary).into());
    let entries = (block.entries.iter())
        .filter(|entry| !matches!(entry, Entry::Log(r) if r.level > level))
        .map(view_entry)
        .chain(interrupted)
        .chain(private);
    column([
        text(format!("> {}", block.line)).font(BOLD).into(),
        container(column(entries).spacing(spacing()))
            .padding(padding::left(2.0 * spacing()))
            .into(),
    ])
    .spacing(spacing())
    .into()
}

fn view_entry(entry: &Entry) -> Element<'_, Message> {
    match entry {
        Entry::Text(message) => text(message).into(),
        Entry::Log(record) => view_record(record),
        Entry::Live { id, island, .. } => {
            let id = *id;
            island.view().map(move |m| Message::Island(id, m))
        }
        Entry::Frozen(island) => view_frozen(island),
        Entry::Unreadable { error, .. } => text(format!("Couldn't restore this island: {error}"))
            .style(text::danger)
            .into(),
    }
}

/// Shows a log record as `level: message`, coloured by level.
fn view_record(record: &LogRecord) -> Element<'_, Message> {
    let style = match record.level {
        Level::Error => text::danger,
        Level::Warn => text::warning,
        Level::Info => text::default,
        Level::Debug => text::secondary,
    };
    text(record.to_string()).style(style).into()
}

#[cfg(test)]
mod tests;
