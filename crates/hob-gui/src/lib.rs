//! The hob shell: owns the main window, runs commands, and shows kettle-ui
//! islands in it.
//!
//! The shell itself is plain iced. It works like a terminal: a scrollback,
//! with a command prompt below it. Each command gets a block in the
//! scrollback: its command line, then what it prints and the islands it
//! shows, in order. A live island freezes in place once the user submits or
//! cancels it.
//!
//! The prompt works like a shell's: Up and Down browse the history, and Tab
//! completes command names.
//!
//! The history and scrollback are saved as a [`Session`] after every change
//! and on exit, and restored on the next start.

use std::fmt;
use std::mem;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use hob_core::{History, Interpreter, Reply, Request, Session};
use iced::keyboard::{self, key};
use iced::widget::{Id, column, container, operation, rule, scrollable, text, text_input};
use iced::{Element, Event, Font, Length, Subscription, Task, event, font, padding, window};
use kettle_ui::FrozenIsland;
use kettle_ui_iced::{Island, spacing, view_frozen};

/// Opens the main window, running commands with `interpreter`, and blocks
/// until it closes. If `session` is given, the session is restored from that
/// file, and saved to it.
pub fn run(interpreter: Interpreter, session: Option<PathBuf>) -> iced::Result {
    let interpreter = Arc::new(interpreter);
    iced::application(
        move || Shell::new(interpreter.clone(), session.clone()),
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
    /// Lists the commands, as a hint.
    placeholder: String,
    /// Where the session is saved, if anywhere.
    session: Option<PathBuf>,
    prompt: String,
    history: History,
    /// Completions offered after Tab, shown below the prompt.
    candidates: Vec<String>,
    /// The scrollback: one block per command line, oldest first.
    blocks: Vec<Block>,
    /// For block and island ids.
    next_id: u64,
}

type BlockId = u64;
type IslandId = u64;

/// A command line and its output.
struct Block {
    id: BlockId,
    line: String,
    entries: Vec<Entry>,
}

/// Something a command showed.
enum Entry {
    Text(String),
    Live {
        id: IslandId,
        island: Island,
        reply: Reply,
    },
    Frozen(FrozenIsland),
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
    fn new(interpreter: Arc<Interpreter>, session: Option<PathBuf>) -> (Self, Task<Message>) {
        let commands: Vec<_> = interpreter.commands().collect();
        let restored = session
            .as_deref()
            .and_then(load_session)
            .unwrap_or_default();
        let mut shell = Shell {
            placeholder: format!("Command ({})", commands.join(", ")),
            interpreter,
            session,
            prompt: String::new(),
            history: History::from(restored.history),
            candidates: Vec::new(),
            blocks: Vec::new(),
            next_id: 0,
        };
        for block in restored.blocks {
            let id = shell.next_id();
            let entries = (block.entries.into_iter())
                .map(|entry| match entry {
                    hob_core::Entry::Text(text) => Entry::Text(text),
                    hob_core::Entry::Island(island) => Entry::Frozen(island),
                })
                .collect();
            shell.blocks.push(Block {
                id,
                line: block.line,
                entries,
            });
        }
        (shell, operation::focus(Id::new(PROMPT)))
    }

    /// Saves the history and scrollback, with live islands frozen as they are
    /// now. Errors are reported on stderr.
    fn save(&self) {
        let Some(path) = &self.session else {
            return;
        };
        let blocks = (self.blocks.iter())
            .map(|block| hob_core::Block {
                line: block.line.clone(),
                entries: (block.entries.iter())
                    .map(|entry| match entry {
                        Entry::Text(text) => hob_core::Entry::Text(text.clone()),
                        Entry::Live { island, .. } => hob_core::Entry::Island(island.freeze()),
                        Entry::Frozen(island) => hob_core::Entry::Island(island.clone()),
                    })
                    .collect(),
            })
            .collect();
        let session = Session {
            history: self.history.lines().to_vec(),
            blocks,
        };
        if let Err(e) = session.save(path) {
            eprintln!("hob: couldn't save the session to {}: {e}", path.display());
        }
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
                    let task = self.handle(block, request);
                    self.save();
                    task
                }
                None => Task::none(),
            },
            Message::Island(id, message) => self.update_island(id, message),
            Message::CloseRequested => {
                self.save();
                iced::exit()
            }
        }
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
        };
        let task = match self.interpreter.run(&block.line) {
            Ok(requests) => Task::run(requests, move |r| Message::Request(id, Pending::new(r))),
            Err(e) => {
                block.entries.push(Entry::Text(e.to_string()));
                Task::none()
            }
        };
        self.blocks.push(block);
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
        let island_id = self.next_id();
        let Some(block) = self.blocks.iter_mut().find(|b| b.id == block) else {
            return Task::none();
        };
        let entry = match request {
            Request::Print(message) => {
                // Also on stdout, e.g. for scripts.
                println!("{message}");
                Entry::Text(message)
            }
            Request::PrintIsland(island) => Entry::Frozen(island),
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

    fn update_island(&mut self, id: IslandId, message: kettle_ui_iced::Message) -> Task<Message> {
        let entry = self
            .blocks
            .iter_mut()
            .flat_map(|b| &mut b.entries)
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
        let scrollback = column(self.blocks.iter().map(view_block)).spacing(2.0 * spacing());

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

fn view_block(block: &Block) -> Element<'_, Message> {
    let bold = Font {
        weight: font::Weight::Bold,
        ..Font::DEFAULT
    };
    let entries = column(block.entries.iter().map(view_entry)).spacing(spacing());
    column([
        text(format!("> {}", block.line)).font(bold).into(),
        container(entries)
            .padding(padding::left(2.0 * spacing()))
            .into(),
    ])
    .spacing(spacing())
    .into()
}

fn view_entry(entry: &Entry) -> Element<'_, Message> {
    match entry {
        Entry::Text(message) => text(message).into(),
        Entry::Live { id, island, .. } => {
            let id = *id;
            island.view().map(move |m| Message::Island(id, m))
        }
        Entry::Frozen(island) => view_frozen(island),
    }
}

/// Loads the saved session, if there is one. A file that can't be read is
/// reported on stderr and moved aside, so it isn't overwritten.
fn load_session(path: &std::path::Path) -> Option<Session> {
    match Session::load(path) {
        Ok(session) => session,
        Err(e) => {
            let mut aside = path.as_os_str().to_owned();
            aside.push(".bad");
            let aside = PathBuf::from(aside);
            let moved = match std::fs::rename(path, &aside) {
                Ok(()) => format!("moved it to {}", aside.display()),
                Err(e) => format!("couldn't move it aside: {e}"),
            };
            eprintln!(
                "hob: couldn't restore the session from {}: {e}; {moved}",
                path.display()
            );
            None
        }
    }
}
