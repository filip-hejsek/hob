//! The hob shell: owns the main window, runs commands, and shows kettle-ui
//! islands in it.
//!
//! The shell itself is plain iced. It works like a terminal: a scrollback,
//! with a command prompt below it. Each command gets a block in the
//! scrollback: its command line, then what it prints and the islands it
//! shows, in order. A live island freezes in place once the user submits or
//! cancels it.

use std::fmt;
use std::mem;
use std::sync::{Arc, Mutex};

use hob_core::{Interpreter, Reply, Request};
use iced::widget::{Id, column, container, operation, rule, scrollable, text, text_input};
use iced::{Element, Font, Length, Task, font, padding};
use kettle_ui::FrozenIsland;
use kettle_ui_iced::{Island, spacing, view_frozen};

/// Opens the main window, running commands with `interpreter`, and blocks
/// until it closes.
pub fn run(interpreter: Interpreter) -> iced::Result {
    let interpreter = Arc::new(interpreter);
    iced::application(
        move || Shell::new(interpreter.clone()),
        Shell::update,
        Shell::view,
    )
    .title("hob")
    .window_size((640.0, 560.0))
    .run()
}

const PROMPT: &str = "prompt";

struct Shell {
    interpreter: Arc<Interpreter>,
    /// Lists the commands, as a hint.
    placeholder: String,
    prompt: String,
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
    /// A request from the command of a block.
    Request(BlockId, Pending<Request>),
    Island(IslandId, kettle_ui_iced::Message),
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
    fn new(interpreter: Arc<Interpreter>) -> (Self, Task<Message>) {
        let commands: Vec<_> = interpreter.commands().collect();
        let shell = Shell {
            placeholder: format!("Command ({})", commands.join(", ")),
            interpreter,
            prompt: String::new(),
            blocks: Vec::new(),
            next_id: 0,
        };
        (shell, operation::focus(Id::new(PROMPT)))
    }

    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::PromptChanged(prompt) => {
                self.prompt = prompt;
                Task::none()
            }
            Message::PromptSubmitted => self.run_prompt(),
            Message::Request(block, request) => match request.take() {
                Some(request) => self.handle(block, request),
                None => Task::none(),
            },
            Message::Island(id, message) => self.update_island(id, message),
        }
    }

    fn run_prompt(&mut self) -> Task<Message> {
        if self.prompt.trim().is_empty() {
            return Task::none();
        }
        let line = mem::take(&mut self.prompt);
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
            container(prompt).padding(spacing()).into(),
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
