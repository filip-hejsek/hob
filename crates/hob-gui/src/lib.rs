//! The hob shell: owns the main window, runs commands, and shows kettle-ui
//! islands in it.
//!
//! The shell itself is plain iced. It works like a terminal: a scrollback,
//! with a command prompt below it. Each command gets a block in the
//! scrollback: its command line, then what it prints, logs and the islands it
//! shows, in order. A live island freezes in place once the user submits or
//! cancels it. The `clear` command clears the scrollback, except for
//! commands still running.
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
//! completes command names.
//!
//! The history and scrollback are saved as a [`Session`] after every change
//! and on exit, and restored on the next start. Commands still running on
//! exit are marked as interrupted.

use std::fmt;
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hob_core::{History, Interpreter, Level, LogRecord, Reply, Request, Session, SessionError};
use iced::keyboard::{self, key};
use iced::widget::{Id, column, container, operation, rule, scrollable, text, text_input};
use iced::{Element, Event, Font, Length, Subscription, Task, event, font, padding, window};
use kettle_ui::FrozenIsland;
use kettle_ui_iced::{Island, spacing, view_frozen};

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
    let placeholder = placeholder.into();
    let level = hob_core::logger::level_from_env();
    // The boot function is `Fn`, but it's only called once.
    let records = Mutex::new(hob_core::logger::install(level));
    let interpreter = Arc::new(interpreter);
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
    .run()
}

const PROMPT: &str = "prompt";

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
    /// A key for the prompt was pressed, maybe while it isn't focused.
    PromptKey(PromptKey),
    /// A key for the prompt was pressed while it was focused.
    PromptKeyFocused(PromptKey),
    /// A request from the command of a block.
    Request(BlockId, Pending<Request>),
    Island(IslandId, kettle_ui_iced::Message),
    /// The command of a block finished.
    Finished(BlockId),
    /// A record from the `log` crate.
    Log(LogRecord),
    CloseRequested,
}

/// A key the prompt handles that iced's text input doesn't.
#[derive(Clone, Copy, Debug)]
enum PromptKey {
    Up,
    Down,
    Tab,
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
    /// now, and without ephemeral log records.
    ///
    /// An error is shown once, as an ephemeral notice: it can't be saved,
    /// and saving it mustn't cause another save.
    fn save(&mut self) {
        let Some(path) = &self.session else {
            return;
        };
        let session = Session {
            history: self.history.lines().to_vec(),
            scrollback: self.scrollback.iter().map(Item::to_saved).collect(),
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
            Message::PromptKey(key) => {
                operation::is_focused(Id::new(PROMPT)).then(move |focused| {
                    if focused {
                        Task::done(Message::PromptKeyFocused(key))
                    } else {
                        Task::none()
                    }
                })
            }
            Message::PromptKeyFocused(key) => self.prompt_key(key),
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
            Message::CloseRequested => {
                self.save();
                iced::exit()
            }
        }
    }

    fn block_mut(&mut self, id: BlockId) -> Option<&mut Block> {
        self.scrollback.iter_mut().find_map(|item| match item {
            Item::Block(block) if block.id == id => Some(block),
            _ => None,
        })
    }

    fn run_prompt(&mut self) -> Task<Message> {
        if self.prompt.trim().is_empty() {
            return Task::none();
        }
        let line = mem::take(&mut self.prompt);
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

    fn prompt_key(&mut self, key: PromptKey) -> Task<Message> {
        let line = match key {
            PromptKey::Up => self.history.back(&self.prompt).map(str::to_owned),
            PromptKey::Down => self.history.forward().map(str::to_owned),
            PromptKey::Tab => {
                let completion = self.interpreter.complete(&self.prompt);
                self.candidates = completion.candidates;
                self.history.stop_browsing();
                Some(completion.line)
            }
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
        let keys = event::listen_with(|event, _status, _window| {
            let Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event else {
                return None;
            };
            if !modifiers.is_empty() {
                return None;
            }
            let key = match key {
                keyboard::Key::Named(key::Named::ArrowUp) => PromptKey::Up,
                keyboard::Key::Named(key::Named::ArrowDown) => PromptKey::Down,
                keyboard::Key::Named(key::Named::Tab) => PromptKey::Tab,
                _ => return None,
            };
            Some(Message::PromptKey(key))
        });
        Subscription::batch([close, keys])
    }

    fn handle(&mut self, block: BlockId, request: Request) -> Task<Message> {
        let request = match request {
            Request::LogRecords(reply) => {
                reply.send(self.log_records());
                return Task::none();
            }
            Request::Clear => {
                self.clear(block);
                return Task::none();
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
            Request::LogRecords(_) | Request::Clear => unreachable!("handled above"),
            Request::Show(show) => {
                let (builder, reply) = show.into_parts();
                match Island::new(builder) {
                    Ok((island, task)) => {
                        block.entries.push(Entry::Live {
                            id: island_id,
                            island,
                            reply,
                        });
                        return task.map(move |m| Message::Island(island_id, m));
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

    /// Clears the scrollback, except for blocks of commands still running
    /// other than `caller`'s. Their log records are kept for `messages`.
    fn clear(&mut self, caller: BlockId) {
        let (kept, cleared) = mem::take(&mut self.scrollback)
            .into_iter()
            .partition(|item| matches!(item, Item::Block(b) if b.running && b.id != caller));
        self.scrollback = kept;
        self.cleared_records.extend(log_records(&cleared));
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
        }
        task.map(move |m| Message::Island(id, m))
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
            .on_submit(Message::PromptSubmitted);

        column([
            scrollable(container(scrollback).padding(2.0 * spacing()))
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
            .padding(spacing())
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

/// Shows a block; log records below `level` are hidden.
fn view_block(block: &Block, level: Level) -> Element<'_, Message> {
    let bold = Font {
        weight: font::Weight::Bold,
        ..Font::DEFAULT
    };
    let note = block.interrupted.then(|| {
        text("Interrupted: hob exited while this command was running.")
            .style(text::secondary)
            .into()
    });
    let entries = (block.entries.iter())
        .filter(|entry| !matches!(entry, Entry::Log(r) if r.level > level))
        .map(view_entry)
        .chain(note);
    column([
        text(format!("> {}", block.line)).font(bold).into(),
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
mod tests {
    use super::*;

    #[test]
    fn dependency_warnings_are_not_shown() {
        let shown = |level, target| shows_notice(&LogRecord::new(level, target, "m"), Level::Info);
        assert!(shown(Level::Warn, "hob_gui"));
        assert!(!shown(Level::Info, "hob_gui"));
        assert!(shown(Level::Error, "wgpu_core"));
        assert!(!shown(Level::Warn, "iced_winit::proxy"));
        let record = LogRecord::new(Level::Warn, "hob_gui", "m");
        assert!(!shows_notice(&record, Level::Error));
    }
}
