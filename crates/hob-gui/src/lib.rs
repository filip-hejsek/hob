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
//! The prompt works like a shell's: Enter runs the command line (it has one
//! line; long lines wrap, and pasted line breaks become spaces). Up and Down
//! browse the history, and Tab completes command names and arguments. Tab again, while candidates are
//! listed, picks one (Tab and Shift+Tab move on, letters and digits search
//! the candidates, Shift+Space takes the picked one without a space, Escape
//! goes back to the line from before). Tab doesn't move focus. Anywhere, PageUp and
//! PageDown scroll the scrollback, and Ctrl+L clears it (like `clear`). Ctrl+D
//! in the empty prompt, or while nothing has focus, exits (like `exit`), unless
//! commands are running: then it says so, and focuses an island waiting for
//! input. Typing while nothing has focus types into the prompt. The bindings are a table (see the
//! `keys` module), to make them configurable later.
//!
//! Command lines and output can be selected with the mouse and copied with
//! Ctrl+C, one printed text or log record at a time.
//!
//! A newly shown island gets focus, unless the user is busy elsewhere
//! (something other than the empty prompt has focus). When an island
//! freezes and nothing has focus, the prompt gets it back. Islands handle
//! some keys themselves (see [`kettle_ui_iced`]).
//!
//! The history and scrollback are saved as a [`Session`] after every change
//! and on exit, and restored on the next start. Before saving, they're
//! trimmed to [`Limits`] (see [`Limits::from_env`]): the oldest items of the
//! scrollback are dropped when it gets too big, and older items lose their
//! images' data when the images do. Commands still running on
//! exit are marked as interrupted. A command line starting with a space is
//! private: neither it nor its output is saved. It stays in the history
//! while its block is in the scrollback.
//!
//! [`preview`] shows a saved session read-only, without an interpreter: there
//! is no prompt, nothing runs, and the session file is never written.

use std::cmp::Reverse;
use std::fmt;
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hob_core::{
    Choice, History, Interpreter, Level, Limits, LogRecord, Reply, Request, Session, SessionError,
    Trim,
};
use iced::advanced::widget::operation::{
    Focusable, Outcome as OperationOutcome, Scrollable, focusable,
};
use iced::advanced::widget::{Operation, operate};
use iced::keyboard::{self, Modifiers};
use iced::widget::scrollable::AbsoluteOffset;
use iced::widget::text_editor::{self, Content, Edit, KeyPress, Motion};
use iced::widget::{Id, column, container, operation, rich_text, row, rule, span, text};
use iced::{Element, Event, Rectangle, Subscription, Task, Theme, event, padding, window};
use kettle_ui::FrozenIsland;
use kettle_ui_iced::{Island, spacing, view_frozen};
use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Matcher, Utf32Str};

use keyed::keyed;
use keys::{Action, Binding, Scope};
use prompt::{BOLD, prompt};
use selectable::selectable;
use sticky::sticky;

mod keyed;
mod keys;
mod prompt;
mod selectable;
mod sticky;

/// The target of the shell's own log records.
const TARGET: &str = "hob_gui";

/// The name of the program that saved a session without a name: such
/// sessions are older than names, and were all saved by hob.
const UNNAMED_SESSION: &str = "hob";

/// Configuration supplied by the application running the shell.
#[derive(Clone, Debug)]
pub struct GuiConfig {
    /// The window's title.
    pub title: String,
    /// The window's Wayland app ID and X11 class, which should match the
    /// name of the desktop file.
    pub app_id: String,
    /// The placeholder shown in the empty prompt, e.g. a hint on how to list
    /// the commands.
    pub command_placeholder: String,
    /// Starts the shell's messages on stderr, followed by `: `.
    pub log_prefix: String,
    /// Command lines run on start, in order, as if typed in the prompt (but
    /// not in a preview).
    pub startup: Vec<String>,
}

/// Opens the main window, running commands with `interpreter`, and blocks
/// until it closes. If `session` is given, the session is restored from that
/// file, and saved to it.
pub fn run(interpreter: Interpreter, session: Option<PathBuf>, config: GuiConfig) -> iced::Result {
    let early_log = hob_core::logger::early_log_stderr(&config.log_prefix);
    let level = hob_core::logger::level_from_env(early_log);
    let records = hob_core::logger::install(level, config.log_prefix.clone());
    let limits = Limits::from_env();
    application(
        Some(Arc::new(interpreter)),
        session,
        limits,
        config,
        level,
        records,
    )
    .run()
}

/// Opens the session saved in `session` read-only, and blocks until the
/// window closes. No commands run, and the file isn't changed, even if it
/// can't be read. The program's name is the one saved in the session.
pub fn preview(session: PathBuf, config: GuiConfig) -> iced::Result {
    let early_log = hob_core::logger::early_log_stderr(&config.log_prefix);
    let level = hob_core::logger::level_from_env(early_log);
    let records = hob_core::logger::install(level, config.log_prefix.clone());
    let config = GuiConfig {
        title: format!("{} (preview of {})", config.title, session.display()),
        ..config
    };
    let limits = Limits::default();
    application(None, Some(session), limits, config, level, records).run()
}

/// The shell as an iced application, ready to run. Without an interpreter,
/// it's a read-only preview of the session. The session is trimmed to
/// `limits` when saved. The window is set up as `config` says. Log records
/// that aren't from a command are read from `records`, if given.
fn application(
    interpreter: Option<Arc<Interpreter>>,
    session: Option<PathBuf>,
    limits: Limits,
    config: GuiConfig,
    level: Level,
    records: Option<futures_channel::mpsc::UnboundedReceiver<LogRecord>>,
) -> iced::Application<impl iced::Program<State = Shell, Message = Message, Theme = iced::Theme>> {
    let title = config.title.clone();
    let window = window_settings(&config.app_id);
    let config = Arc::new(config);
    // The boot function is `Fn`, but it's only called once.
    let records = Mutex::new(records);
    iced::application(
        move || {
            let records = records.lock().unwrap().take();
            Shell::new(
                interpreter.clone(),
                session.clone(),
                limits,
                config.clone(),
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
    .title(move |_: &Shell| title.clone())
    .window(window)
}

#[cfg_attr(not(target_os = "linux"), expect(unused_variables))]
fn window_settings(app_id: &str) -> iced::window::Settings {
    iced::window::Settings {
        size: iced::Size::new(640.0, 560.0),
        #[cfg(target_os = "linux")]
        platform_specific: iced::window::settings::PlatformSpecific {
            application_id: app_id.to_owned(),
            ..Default::default()
        },
        ..Default::default()
    }
}

const PROMPT: &str = "prompt";
const SCROLLBACK: &str = "scrollback";

struct Shell {
    /// Runs commands; without one, the shell is a read-only preview.
    interpreter: Option<Arc<Interpreter>>,
    /// The program's name, as the interpreter has it (or, in a preview, as
    /// the session has it), e.g. for notes.
    name: String,
    /// Configuration provided by the application.
    config: Arc<GuiConfig>,
    /// Where the session is saved, if anywhere.
    session: Option<PathBuf>,
    /// How big the saved session may get; the scrollback and history are
    /// trimmed to fit.
    limits: Limits,
    /// The last error saving the session, so it's reported only once.
    save_error: Option<String>,
    /// The lowest level of log records shown.
    level: Level,
    /// The prompt's text and cursor.
    prompt: Content,
    history: History,
    /// The candidates Tab found, if there are several; shown below the
    /// prompt.
    offer: Option<Offer>,
    /// The key bindings.
    bindings: Vec<Binding>,
    /// The scrollback, oldest first.
    scrollback: Vec<Item>,
    /// Log records of cleared items, kept for `messages`, but not saved.
    cleared_records: Vec<LogRecord>,
    /// For block and island ids.
    next_id: u64,
    /// Changed to scroll the scrollback to the bottom, dropping the empty
    /// space left there when content got shorter.
    reveal: u64,
}

type BlockId = u64;
type IslandId = u64;

/// Something in the scrollback.
enum Item {
    Block(Block),
    /// A log record that isn't from a command.
    Notice(LogRecord),
}

/// Candidates offered by Tab completion. Tab again starts picking one: the
/// picked candidate is filled in to the prompt, and Tab and Shift+Tab pick
/// another. While picking, letters and digits search the candidates (fuzzy,
/// best match first, picked), and Backspace takes them back. Escape
/// restores the line from before; Shift+Space takes the candidate as it is;
/// anything else that changes the line ends the offer (so Space and Enter
/// take the candidate); typing a candidate's delimiter (`/` after a
/// directory) goes on as completing it alone would.
struct Offer {
    /// The candidates, and the line with each filled in.
    choices: Vec<Choice>,
    /// What's been typed to search the candidates while picking.
    query: String,
    /// The candidates matching the query, best first; all of them, in
    /// order, without one.
    matches: Vec<Match>,
    /// The match picked, while picking (unless none matches).
    picked: Option<usize>,
    /// The line from before picking, while picking.
    before: Option<String>,
}

/// A candidate matching an [`Offer`]'s query.
struct Match {
    /// Which of the offer's choices.
    index: usize,
    /// The positions of the chars of its listed name that match, ascending.
    positions: Vec<usize>,
}

impl Offer {
    fn new(choices: Vec<Choice>) -> Self {
        let mut offer = Offer {
            choices,
            query: String::new(),
            matches: Vec::new(),
            picked: None,
            before: None,
        };
        offer.search();
        offer
    }

    /// Whether a candidate is being picked (or searched for, even if none
    /// matches).
    fn picking(&self) -> bool {
        self.before.is_some()
    }

    /// Finds the candidates matching the query, best first, ties in order,
    /// and while picking, picks the best.
    fn search(&mut self) {
        self.matches = if self.query.is_empty() {
            (0..self.choices.len())
                .map(|index| Match {
                    index,
                    positions: Vec::new(),
                })
                .collect()
        } else {
            let atom = Atom::new(
                &self.query,
                CaseMatching::Smart,
                Normalization::Smart,
                AtomKind::Fuzzy,
                false,
            );
            let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
            let (mut chars, mut indices) = (Vec::new(), Vec::new());
            let mut scored: Vec<(u16, Match)> = (self.choices.iter().enumerate())
                .filter_map(|(index, choice)| {
                    // Without its delimiter (the `/` of a directory).
                    let name = (choice.continued.as_ref())
                        .and_then(|(d, _)| choice.shown.strip_suffix(*d))
                        .unwrap_or(&choice.shown);
                    indices.clear();
                    let haystack = Utf32Str::new(name, &mut chars);
                    let score = atom.indices(haystack, &mut matcher, &mut indices)?;
                    indices.sort_unstable();
                    indices.dedup();
                    let positions = indices.iter().map(|&i| i as usize).collect();
                    Some((score, Match { index, positions }))
                })
                .collect();
            scored.sort_by_key(|(score, _)| Reverse(*score));
            scored.into_iter().map(|(_, m)| m).collect()
        };
        if self.picking() {
            self.picked = (!self.matches.is_empty()).then_some(0);
        }
    }

    /// The picked choice, if any.
    fn picked(&self) -> Option<&Choice> {
        let m = &self.matches[self.picked?];
        Some(&self.choices[m.index])
    }

    /// The line to show while picking: the picked candidate filled in, or
    /// if none matches, the line from before with the query typed after it.
    fn line(&self) -> Option<String> {
        match self.picked() {
            Some(choice) => Some(choice.filled.clone()),
            None => Some(format!("{}{}", self.before.as_ref()?, self.query)),
        }
    }

    /// The line with `c` typed after the picked candidate, if `c` is its
    /// delimiter.
    fn continued(&self, c: char) -> Option<String> {
        match &self.picked()?.continued {
            Some((delimiter, line)) if *delimiter == c => Some(line.clone()),
            _ => None,
        }
    }
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
    /// The user did something in the prompt.
    PromptEdited(text_editor::Action),
    PromptSubmitted,
    /// A key was pressed (the shell's bindings decide what it does).
    Key(keyboard::Key, Modifiers),
    /// Text was typed, and no widget took it.
    Typed(String),
    /// Text was typed while nothing had focus; it goes to the prompt.
    TypedUnfocused(String),
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
        interpreter: Option<Arc<Interpreter>>,
        session: Option<PathBuf>,
        limits: Limits,
        config: Arc<GuiConfig>,
        level: Level,
        records: Option<futures_channel::mpsc::UnboundedReceiver<LogRecord>>,
    ) -> (Self, Task<Message>) {
        let name = interpreter.as_ref().map(|i| i.name().to_owned());
        let mut shell = Shell {
            config,
            interpreter,
            name: name.unwrap_or_else(|| UNNAMED_SESSION.to_owned()),
            session,
            limits,
            save_error: None,
            level,
            prompt: Content::new(),
            history: History::new(),
            offer: None,
            bindings: keys::defaults(),
            scrollback: Vec::new(),
            cleared_records: Vec::new(),
            next_id: 0,
            reveal: 0,
        };
        shell.restore();
        let mut tasks = Vec::new();
        if !shell.is_preview() {
            tasks.push(operation::focus(Id::new(PROMPT)));
            let startup = shell.config.startup.clone();
            for line in &startup {
                tasks.push(shell.run_line(line.clone()));
            }
            if !startup.is_empty() {
                shell.save();
            }
        }
        if let Some(records) = records {
            tasks.push(Task::run(records, Message::Log));
        }
        (shell, Task::batch(tasks))
    }

    /// Whether this is a read-only preview of a session, without an
    /// interpreter.
    fn is_preview(&self) -> bool {
        self.interpreter.is_none()
    }

    /// Restores the saved session, if there is one. Errors are reported; a
    /// file with bad contents is moved aside, so it isn't overwritten (but
    /// not in a preview, which never writes it).
    ///
    /// A preview takes the program's name from the session. Otherwise, a
    /// session saved by another program is warned about; the warning is
    /// saved with the next change, not right away.
    fn restore(&mut self) {
        let Some(path) = self.session.clone() else {
            return;
        };
        let restored = match Session::load(&path) {
            Ok(Some(session)) => session,
            Ok(None) if self.is_preview() => {
                let message = format!("There's no saved session at {}.", path.display());
                let notice = self.notice(Level::Error, message);
                self.scrollback.push(Item::Notice(notice));
                return;
            }
            Ok(None) => return,
            Err(e) => {
                let mut message =
                    format!("Couldn't restore the session from {}: {e}", path.display());
                // The file was read, but its contents are no good.
                if !matches!(e, SessionError::Io(_)) && !self.is_preview() {
                    match move_aside(&path) {
                        Ok(aside) => message += &format!("; moved it to {}", aside.display()),
                        Err(e) => message += &format!("; couldn't move it aside: {e}"),
                    }
                }
                let notice = self.notice(Level::Error, message);
                self.scrollback.push(Item::Notice(notice));
                return;
            }
        };
        let other_name = match restored.name {
            Some(name) if self.is_preview() => {
                self.name = name;
                None
            }
            Some(name) if name != self.name => Some(name),
            _ => None,
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
        if let Some(other_name) = other_name {
            let message = format!("a session from {other_name} was loaded into {}", self.name);
            let notice = self.notice(Level::Warn, message);
            self.scrollback.push(Item::Notice(notice));
        }
    }

    /// Saves the history and scrollback, with live islands frozen as they are
    /// now, and without ephemeral log records or private blocks. Both are
    /// trimmed first to keep the file within the limits (see
    /// [`trim`](Shell::trim)).
    ///
    /// An error is shown once, as an ephemeral notice: it can't be saved,
    /// and saving it mustn't cause another save. A preview saves nothing.
    fn save(&mut self) {
        let Some(path) = self.session.clone() else {
            return;
        };
        if self.is_preview() {
            return;
        }
        self.history.truncate(self.limits.history);
        let (indices, mut scrollback) = self.saved_items();
        let trim = Trim::new(&scrollback, &self.limits);
        if !trim.is_empty() {
            self.trim(&indices, trim);
            (_, scrollback) = self.saved_items();
        }
        let session = Session {
            name: Some(self.name.clone()),
            history: self.history.saved_lines(),
            scrollback,
        };
        let notice = match session.save(&path) {
            Ok(()) => match self.save_error.take() {
                Some(_) => LogRecord::new(Level::Info, TARGET, "Saving the session works again."),
                None => return,
            },
            Err(e) => {
                let message = format!("Couldn't save the session to {}: {e}", path.display());
                if self.save_error.as_ref() == Some(&message) {
                    return;
                }
                self.save_error = Some(message.clone());
                self.notice(Level::Error, message).ephemeral()
            }
        };
        self.scrollback.push(Item::Notice(notice));
    }

    /// A notice of the shell's own, also printed to stderr. Not logged with
    /// the `log` crate, since its records come back as notices that aren't
    /// ephemeral, and later than the shell's own.
    fn notice(&self, level: Level, message: String) -> LogRecord {
        let record = LogRecord::new(level, TARGET, message);
        eprintln!("{}: {record}", self.config.log_prefix);
        record
    }

    /// The items of the scrollback that are saved, as saved, with their
    /// indices in the scrollback: all but private blocks.
    fn saved_items(&self) -> (Vec<usize>, Vec<hob_core::Item>) {
        (self.scrollback.iter().enumerate())
            .filter(|(_, item)| !matches!(item, Item::Block(b) if History::is_private(&b.line)))
            .map(|(i, item)| (i, item.to_saved()))
            .unzip()
    }

    /// Trims the scrollback as `trim` says, for the saved items at
    /// `indices`. Items between them (private blocks) go with the saved
    /// item before them. Blocks of commands still running aren't dropped,
    /// since their commands still use them, nor are live islands' images.
    ///
    /// Dropped items are gone, unlike with `clear`: their log records
    /// aren't kept for `messages`, since the point is to use less space.
    fn trim(&mut self, indices: &[usize], trim: Trim) {
        // The newest saved item is always kept, so the indices are there.
        let images_from = indices[trim.images_from];
        for item in &mut self.scrollback[..images_from] {
            if let Item::Block(block) = item {
                for entry in &mut block.entries {
                    if let Entry::Frozen(island) = entry {
                        *island = island.map_images(hob_core::strip_image);
                    }
                }
            }
        }
        let keep_from = indices[trim.keep_from];
        let mut i = 0;
        self.scrollback.retain(|item| {
            i += 1;
            i > keep_from || matches!(item, Item::Block(b) if b.running)
        });
        self.forget_private_lines();
    }

    /// Forgets private lines from the history whose blocks are gone from the
    /// scrollback.
    fn forget_private_lines(&mut self) {
        let shown: Vec<&str> = (self.scrollback.iter())
            .filter_map(|item| match item {
                Item::Block(block) => Some(block.line.as_str()),
                Item::Notice(_) => None,
            })
            .collect();
        self.history.retain_private(|line| shown.contains(&line));
    }

    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::PromptEdited(action) => {
                use text_editor::Action::Edit as A;
                let action = match action {
                    // The command line has one line.
                    A(Edit::Enter) => return Task::none(),
                    A(Edit::Paste(text)) => A(Edit::Paste(Arc::new(one_line(&text)))),
                    action => action,
                };
                // While picking, letters and digits search the candidates.
                if let Some(offer) = self.offer.as_mut().filter(|o| o.picking()) {
                    let searched = match action {
                        A(Edit::Insert(c)) if c.is_alphanumeric() => {
                            offer.query.push(c);
                            true
                        }
                        A(Edit::Backspace) => offer.query.pop().is_some(),
                        _ => false,
                    };
                    if searched {
                        offer.search();
                        if let Some(line) = offer.line() {
                            self.set_prompt(&line);
                        }
                        return Task::none();
                    }
                }
                if action.is_edit() {
                    self.history.stop_browsing();
                    let offer = self.offer.take();
                    // Typing the picked candidate's delimiter goes on after it.
                    if let A(Edit::Insert(c)) = action
                        && let Some(line) = offer.and_then(|o| o.continued(c))
                    {
                        self.set_prompt(&line);
                        return Task::none();
                    }
                }
                self.prompt.perform(action);
                Task::none()
            }
            Message::PromptSubmitted => {
                let task = self.run_prompt();
                self.save();
                task
            }
            Message::Key(key, modifiers) => match keys::lookup(&self.bindings, key, modifiers) {
                Some(action) => match action.scope() {
                    Scope::Anywhere => self.perform(action),
                    // While the prompt has focus, its key bindings do it.
                    Scope::Prompt => Task::none(),
                    Scope::PromptOrNothing => focused().then(move |focused| match focused {
                        None => Task::done(Message::Action(action)),
                        Some(_) => Task::none(),
                    }),
                },
                None => Task::none(),
            },
            Message::Action(action) => self.perform(action),
            Message::Typed(text) if !self.is_preview() => {
                focused().then(move |focused| match focused {
                    None => Task::done(Message::TypedUnfocused(text.clone())),
                    Some(_) => Task::none(),
                })
            }
            Message::Typed(_) => Task::none(),
            Message::TypedUnfocused(text) => {
                self.prompt
                    .perform(text_editor::Action::Move(Motion::DocumentEnd));
                let paste = text_editor::Action::Edit(Edit::Paste(Arc::new(text)));
                let _ = self.update(Message::PromptEdited(paste));
                operation::focus(Id::new(PROMPT))
            }
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
        // shouldn't reach the command or the history, unless escaped or quoted.
        let text = mem::take(&mut self.prompt).text();
        let line = hob_core::trim_line_end(&text).to_owned();
        self.run_line(line)
    }

    /// Runs `line` in a new block, and adds it to the history.
    fn run_line(&mut self, line: String) -> Task<Message> {
        let Some(interpreter) = self.interpreter.clone() else {
            return Task::none();
        };
        if line.is_empty() {
            return Task::none();
        }
        self.history.push(&line);
        self.offer = None;
        // So its output is seen.
        self.reveal += 1;
        let id = self.next_id();
        let mut block = Block {
            id,
            line,
            entries: Vec::new(),
            running: false,
            interrupted: false,
        };
        let task = match interpreter.run(&block.line) {
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
        // A preview only scrolls; nothing in it changes.
        if self.is_preview() && !matches!(action, Action::PageUp | Action::PageDown) {
            return Task::none();
        }
        let line = match action {
            Action::HistoryBack => {
                self.offer = None;
                self.history.back(&self.prompt.text()).map(str::to_owned)
            }
            Action::HistoryForward => {
                self.offer = None;
                self.history.forward().map(str::to_owned)
            }
            Action::Complete if self.offer.is_some() => self.pick(1),
            Action::Complete => {
                let Some(interpreter) = &self.interpreter else {
                    return Task::none();
                };
                let completion = interpreter.complete(&self.prompt.text());
                self.offer =
                    (!completion.choices.is_empty()).then(|| Offer::new(completion.choices));
                self.history.stop_browsing();
                Some(completion.line)
            }
            Action::PickPrevious => self.pick(-1),
            Action::TakePick => {
                if self.offer.as_ref().is_some_and(Offer::picking) {
                    // The prompt already shows the line.
                    self.offer = None;
                    return Task::none();
                }
                let space = text_editor::Action::Edit(Edit::Insert(' '));
                return self.update(Message::PromptEdited(space));
            }
            Action::CancelPick => {
                let Some(offer) = self.offer.as_mut() else {
                    // Escape unfocuses the prompt, unless a candidate is picked.
                    return operate(focusable::unfocus());
                };
                let Some(before) = offer.before.take() else {
                    return operate(focusable::unfocus());
                };
                offer.query.clear();
                offer.picked = None;
                offer.search();
                self.set_prompt(&before);
                // Escape may have been pressed while nothing had focus.
                return operation::focus(Id::new(PROMPT));
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
        if let Some(line) = line {
            self.set_prompt(&line);
        }
        Task::none()
    }

    /// Replaces the prompt's text, with the cursor at its end.
    fn set_prompt(&mut self, text: &str) {
        self.prompt = Content::with_text(text);
        (self.prompt).perform(text_editor::Action::Move(Motion::DocumentEnd));
    }

    /// Picks the match `step` places after the picked one (before, if
    /// negative), and returns the line with it filled in. If none is picked
    /// yet, starts picking with the first (or, going back, the last).
    fn pick(&mut self, step: isize) -> Option<String> {
        let offer = self.offer.as_mut()?;
        let len = offer.matches.len();
        if len == 0 {
            return None;
        }
        let index = match offer.picked {
            Some(index) => (index as isize + step).rem_euclid(len as isize) as usize,
            None if step < 0 => len - 1,
            None => 0,
        };
        if offer.before.is_none() {
            offer.before = Some(self.prompt.text());
        }
        offer.picked = Some(index);
        offer.line()
    }

    fn subscription(&self) -> Subscription<Message> {
        let close = window::close_requests().map(|_| Message::CloseRequested);
        let keys = event::listen_with(|event, status, _window| key_message(event, status));
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
            Request::Print(message) => Entry::Text(message),
            Request::Log(record) => Entry::Log(record),
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
    /// focus to the newest island waiting for input, if any, scrolling it
    /// into view.
    fn exit_if_idle(&mut self) -> Task<Message> {
        let running: Vec<&Block> = self.running().collect();
        let waiting = (running.iter().rev()).find_map(|block| {
            block.entries.iter().rev().find_map(|e| match e {
                Entry::Live { island, .. } => Some((block, island)),
                _ => None,
            })
        });
        let (named, focus) = match (waiting, running.last()) {
            (Some((block, island)), _) => {
                let focus = island.focus();
                (block, Task::batch([focus, scroll_into_view(island.id())]))
            }
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
        // Otherwise only the empty space left at the bottom would be shown.
        self.reveal += 1;
        self.cleared_records.extend(log_records(&cleared));
        self.forget_private_lines();
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
        // Keyed, so a live island keeps its state (focus, cursor) when `clear`
        // removes blocks before it. Notices are plain text, so they can share
        // a key.
        let items = self.scrollback.iter().filter_map(|item| match item {
            Item::Block(block) => Some((Some(block.id), view_block(block, self.level, &self.name))),
            Item::Notice(record) if shows_notice(record, self.level) => {
                Some((None, view_record(record)))
            }
            // Kept for `messages`.
            Item::Notice(_) => None,
        });
        let scrollback = keyed(items).spacing(2.0 * spacing());
        // It doesn't move what it shows when content changes, except to
        // stay at the bottom.
        let scrollback = sticky(
            container(scrollback).padding(2.0 * spacing()),
            Id::new(SCROLLBACK),
            self.reveal,
        );

        // A preview has no prompt; it says what it is instead.
        if let (true, Some(path)) = (self.is_preview(), &self.session) {
            let note = format!("Read-only preview of {}", path.display());
            return column([
                scrollback.into(),
                rule::horizontal(1).into(),
                container(text(note).style(text::secondary))
                    .padding(padding::vertical(spacing()).horizontal(2.0 * spacing()))
                    .into(),
            ])
            .into();
        }

        let bindings = &self.bindings;
        let prompt = prompt(&self.prompt, &self.config.command_placeholder, |editor| {
            editor
                .id(Id::new(PROMPT))
                .on_action(Message::PromptEdited)
                .key_binding(move |key| prompt_binding(bindings, key))
        });

        column([
            scrollback.into(),
            rule::horizontal(1).into(),
            container(
                // The prompt stays the first child, so it keeps its state
                // (focus) when the candidates appear.
                column(
                    [prompt.into()]
                        .into_iter()
                        .chain(self.offer.as_ref().map(view_offer)),
                )
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

/// The message for a key press that reached the shell: [`Message::Typed`]
/// for printable text no widget took, [`Message::Key`] for other keys.
/// Printable text is never looked up in the bindings.
fn key_message(event: Event, status: event::Status) -> Option<Message> {
    let Event::Keyboard(keyboard::Event::KeyPressed {
        key,
        modifiers,
        text,
        ..
    }) = event
    else {
        return None;
    };
    let command = modifiers.control() || modifiers.alt() || modifiers.logo();
    match text {
        Some(text) if !command && !text.chars().any(char::is_control) => {
            (status == event::Status::Ignored).then(|| Message::Typed(text.into()))
        }
        _ => Some(Message::Key(key, modifiers)),
    }
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
    scroll_by(Amount::Pages(pages))
}

/// How far to scroll; up if positive.
#[derive(Clone, Copy)]
enum Amount {
    /// Pages, each a bit less than the scrollback's height, so some lines
    /// stay in view.
    Pages(f32),
    Pixels(f32),
}

/// Scrolls the scrollback by `amount`.
fn scroll_by<T: Send + 'static>(amount: Amount) -> Task<T> {
    struct ScrollBy(Amount);

    impl<T> Operation<T> for ScrollBy {
        fn scrollable(
            &mut self,
            id: Option<&Id>,
            bounds: Rectangle,
            content_bounds: Rectangle,
            _: iced::Vector,
            state: &mut dyn Scrollable,
        ) {
            if id == Some(&Id::new(SCROLLBACK)) {
                let up = match self.0 {
                    Amount::Pages(pages) => {
                        pages * (bounds.height - 3.0 * spacing()).max(spacing())
                    }
                    Amount::Pixels(pixels) => pixels,
                };
                let offset = AbsoluteOffset { x: 0.0, y: -up };
                state.scroll_by(offset, bounds, content_bounds);
            }
        }

        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<T>)) {
            operate(self);
        }
    }

    operate(ScrollBy(amount))
}

/// Scrolls the scrollback as little as needed to show the focusable widget
/// `target` (e.g. an island), or its top if it doesn't fit.
fn scroll_into_view<T: Send + 'static>(target: Id) -> Task<T> {
    struct Find {
        target: Id,
        /// The visible part of the scrollback's content, in its coordinates.
        view: Option<Rectangle>,
        /// The target's top and bottom, in the content's coordinates.
        found: Option<(f32, f32)>,
        /// The top of the content, where the target is measured from.
        content_top: f32,
    }

    impl Operation<f32> for Find {
        fn scrollable(
            &mut self,
            id: Option<&Id>,
            bounds: Rectangle,
            content_bounds: Rectangle,
            translation: iced::Vector,
            _: &mut dyn Scrollable,
        ) {
            if id == Some(&Id::new(SCROLLBACK)) {
                self.view = Some(Rectangle {
                    y: translation.y,
                    ..bounds
                });
                self.content_top = content_bounds.y;
            }
        }

        fn focusable(&mut self, id: Option<&Id>, bounds: Rectangle, _: &mut dyn Focusable) {
            if id == Some(&self.target) {
                let top = bounds.y - self.content_top;
                self.found = Some((top, top + bounds.height));
            }
        }

        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<f32>)) {
            operate(self);
        }

        fn finish(&self) -> OperationOutcome<f32> {
            let (Some(view), Some((top, bottom))) = (self.view, self.found) else {
                return OperationOutcome::Some(0.0);
            };
            // Some room around it, like between blocks.
            let (top, bottom) = (top - 2.0 * spacing(), bottom + 2.0 * spacing());
            let up = if top < view.y {
                view.y - top
            } else if bottom > view.y + view.height {
                -(bottom - (view.y + view.height)).min(top - view.y)
            } else {
                0.0
            };
            OperationOutcome::Some(up)
        }
    }

    operate(Find {
        target,
        view: None,
        found: None,
        content_top: 0.0,
    })
    .then(|up| match up {
        0.0 => Task::none(),
        up => scroll_by(Amount::Pixels(up)),
    })
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

/// The binding of a key pressed in the prompt: the shell's actions for
/// the prompt, Enter to run the line, and otherwise the editor's usual
/// keys. Shift+Enter does nothing: it's kept for a multi-line mode (see
/// planning/TODO.md).
fn prompt_binding(bindings: &[Binding], key: KeyPress) -> Option<text_editor::Binding<Message>> {
    if !matches!(key.status, text_editor::Status::Focused { .. }) {
        return None;
    }
    let custom = |action| Some(text_editor::Binding::Custom(Message::Action(action)));
    match keys::lookup(bindings, key.key.clone(), key.modifiers) {
        // Escape is decided when it's done (it unfocuses, unless a candidate
        // is picked), since keys pressed before the next view would see a
        // stale state here.
        Some(
            action @ (Action::HistoryBack
            | Action::CancelPick
            | Action::HistoryForward
            | Action::Complete
            | Action::PickPrevious
            | Action::TakePick
            | Action::Exit),
        ) => custom(action),
        // The shell does it (e.g. PageUp scrolls the scrollback).
        Some(action) if action.scope() == Scope::Anywhere => None,
        _ => match key.key {
            keyboard::Key::Named(keyboard::key::Named::Enter) => (!key.modifiers.shift())
                .then_some(text_editor::Binding::Custom(Message::PromptSubmitted)),
            _ => text_editor::Binding::from_key_press(key),
        },
    }
}

/// Pasted text for the one-line prompt: line breaks at the ends are dropped
/// (so a leading one doesn't make the line private), and the others become
/// spaces.
fn one_line(text: &str) -> String {
    (text.trim_matches(['\n', '\r']))
        .replace("\r\n", " ")
        .replace(['\n', '\r'], " ")
}

/// Shows a block; log records below `level` are hidden.
/// `name` is the program's name, for the note on interrupted commands.
fn view_block<'a>(block: &'a Block, level: Level, name: &str) -> Element<'a, Message> {
    let interrupted = block.interrupted.then(|| {
        let note = text(format!(
            "Interrupted: {name} exited while this command was running."
        ));
        (None, note.style(text::secondary).into())
    });
    let private = History::is_private(&block.line)
        .then(|| (None, text(PRIVATE_NOTE).style(text::secondary).into()));
    // Keyed by index, so a live island keeps its state when the level
    // hides or shows log records before it. The notes are plain text.
    let entries = (block.entries.iter().enumerate())
        .filter(|(_, entry)| !matches!(entry, Entry::Log(r) if r.level > level))
        .map(|(i, entry)| (Some(i), view_entry(entry)))
        .chain(interrupted)
        .chain(private);
    column([
        selectable(format!("> {}", block.line)).font(BOLD).into(),
        container(keyed(entries).spacing(spacing()))
            .padding(padding::left(2.0 * spacing()))
            .into(),
    ])
    .spacing(spacing())
    .into()
}

/// Lists the candidates of an offer that match its query; the picked one
/// has a box of the primary colour, and the chars matching the query are
/// underlined.
fn view_offer(offer: &Offer) -> Element<'_, Message> {
    if offer.matches.is_empty() {
        let note = format!("no match for \"{}\"", offer.query);
        return text(note).style(text::secondary).into();
    }
    let picked = offer.picked;
    let candidates = offer.matches.iter().enumerate().map(|(i, m)| {
        let choice = &offer.choices[m.index];
        let style = move |theme: &Theme| match picked == Some(i) {
            true => {
                let primary = theme.extended_palette().primary.base;
                container::Style::default()
                    .background(primary.color)
                    .color(primary.text)
            }
            false => container::Style::default(),
        };
        container(highlighted(&choice.shown, &m.positions))
            .padding(padding::horizontal(spacing() / 2.0))
            .style(style)
            .into()
    });
    row(candidates).spacing(spacing()).wrap().into()
}

/// `name`, with the chars at `positions` (ascending) underlined. Not bold:
/// a bold span makes the rest of its word bold too (see ICED_WISHLIST.md).
fn highlighted<'a>(name: &'a str, positions: &[usize]) -> Element<'a, Message> {
    if positions.is_empty() {
        return text(name).into();
    }
    // Runs of chars that match, or don't.
    let mut spans = Vec::new();
    let mut start = 0;
    let mut matching = false;
    let mut positions = positions.iter().peekable();
    for (i, (offset, _)) in name.char_indices().enumerate() {
        let matches = positions.next_if_eq(&&i).is_some();
        if matches != matching {
            spans.push((&name[start..offset], matching));
            (start, matching) = (offset, matches);
        }
    }
    spans.push((&name[start..], matching));
    let spans = (spans.into_iter())
        .filter(|(s, _)| !s.is_empty())
        .map(|(s, matching)| match matching {
            true => span(s).underline(true),
            false => span(s),
        });
    let spans: Vec<text::Span<'_>> = spans.collect();
    rich_text(spans).into()
}

fn view_entry(entry: &Entry) -> Element<'_, Message> {
    match entry {
        Entry::Text(message) => selectable(message).into(),
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
    selectable(record.to_string()).style(style).into()
}

#[cfg(test)]
mod tests;

// The README's examples, compiled as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
