//! The hob shell: owns the main window, runs commands, and shows kettle-ui
//! islands in it.
//!
//! The shell itself is plain iced. For now it has a command prompt, a tab per
//! open island, and a status line that shows what commands print; the active
//! island is shown below them, in a scrollable area.

use std::fmt;
use std::sync::{Arc, Mutex};

use hob_core::{Interpreter, Reply, Request};
use iced::widget::{
    Id, button, column, container, operation, row, rule, scrollable, text, text_input,
};
use iced::{Element, Length, Task};
use kettle_ui_iced::{Island, spacing};

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
    islands: Vec<OpenIsland>,
    active: Option<IslandId>,
    next_id: IslandId,
    status: String,
}

type IslandId = u64;

struct OpenIsland {
    id: IslandId,
    /// The name of the command that opened it.
    title: Arc<str>,
    island: Island,
    reply: Reply,
}

#[derive(Clone, Debug)]
enum Message {
    PromptChanged(String),
    PromptSubmitted,
    /// A request from the named command.
    Request(Arc<str>, Pending<Request>),
    Select(IslandId),
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
            islands: Vec::new(),
            active: None,
            next_id: 0,
            status: String::new(),
        };
        (shell, operation::focus(Id::new(PROMPT)))
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::PromptChanged(prompt) => {
                self.prompt = prompt;
                Task::none()
            }
            Message::PromptSubmitted => self.run_prompt(),
            Message::Request(command, request) => match request.take() {
                Some(request) => self.handle(command, request),
                None => Task::none(),
            },
            Message::Select(id) => {
                self.active = Some(id);
                Task::none()
            }
            Message::Island(id, message) => {
                // Messages can still arrive for an island that was just closed.
                let Some(index) = self.islands.iter().position(|i| i.id == id) else {
                    return Task::none();
                };
                let (task, outcome) = self.islands[index].island.update(message);
                if let Some(outcome) = outcome {
                    self.close(index).reply.send(outcome);
                }
                task.map(move |m| Message::Island(id, m))
            }
        }
    }

    fn run_prompt(&mut self) -> Task<Message> {
        let Some(name) = self.prompt.split_whitespace().next() else {
            return Task::none();
        };
        let name: Arc<str> = name.into();
        match self.interpreter.run(&self.prompt) {
            Ok(requests) => {
                self.prompt.clear();
                self.status.clear();
                Task::run(requests, move |r| {
                    Message::Request(name.clone(), Pending::new(r))
                })
            }
            Err(e) => {
                // Keep the line, so the user can fix it.
                self.print(e.to_string());
                Task::none()
            }
        }
    }

    fn handle(&mut self, command: Arc<str>, request: Request) -> Task<Message> {
        match request {
            Request::Print(message) => {
                self.print(message);
                Task::none()
            }
            Request::Show(show) => {
                let (builder, reply) = show.into_parts();
                let (island, task) = match Island::new(builder) {
                    Ok(built) => built,
                    Err(e) => {
                        reply.fail(e);
                        return Task::none();
                    }
                };
                let id = self.next_id;
                self.next_id += 1;
                self.islands.push(OpenIsland {
                    id,
                    title: command,
                    island,
                    reply,
                });
                self.active = Some(id);
                task.map(move |m| Message::Island(id, m))
            }
        }
    }

    /// Shows a message in the status line, and on stdout.
    fn print(&mut self, message: String) {
        println!("{message}");
        self.status = message;
    }

    fn close(&mut self, index: usize) -> OpenIsland {
        let closed = self.islands.remove(index);
        if self.active == Some(closed.id) {
            let next = self.islands.get(index).or(self.islands.last());
            self.active = next.map(|i| i.id);
        }
        closed
    }

    fn view(&self) -> Element<'_, Message> {
        let prompt = text_input(&self.placeholder, &self.prompt)
            .id(Id::new(PROMPT))
            .on_input(Message::PromptChanged)
            .on_submit(Message::PromptSubmitted);

        let tabs = row(self.islands.iter().map(|open| {
            let style = if self.active == Some(open.id) {
                button::primary
            } else {
                button::text
            };
            button(text(&*open.title))
                .style(style)
                .on_press(Message::Select(open.id))
                .into()
        }))
        .spacing(5);

        let content: Element<'_, Message> = match self.active_island() {
            Some(open) => {
                let id = open.id;
                open.island.view().map(move |m| Message::Island(id, m))
            }
            None => text("No open islands.").into(),
        };

        column([
            column([prompt.into(), tabs.into(), text(&self.status).into()])
                .spacing(spacing())
                .padding(spacing())
                .into(),
            rule::horizontal(1).into(),
            scrollable(container(content).padding(20))
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
        ])
        .into()
    }

    fn active_island(&self) -> Option<&OpenIsland> {
        self.islands.iter().find(|i| Some(i.id) == self.active)
    }
}
