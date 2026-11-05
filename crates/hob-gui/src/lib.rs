//! The hob shell: owns the main window and shows kettle-ui islands in it.
//!
//! The shell itself is plain iced. For now it has a toolbar for opening demo
//! islands, a tab per open island, and a status line; the active island is
//! shown below them, in a scrollable area.

use iced::widget::{button, column, container, row, rule, scrollable, text};
use iced::{Element, Length, Task};
use kettle_ui::Outcome;
use kettle_ui_demo::Demo;
use kettle_ui_iced::{Island, spacing};

/// Opens the main window and blocks until it closes.
pub fn run() -> iced::Result {
    iced::application(Shell::new, Shell::update, Shell::view)
        .title("hob")
        .window_size((640.0, 560.0))
        .run()
}

struct Shell {
    islands: Vec<OpenIsland>,
    active: Option<IslandId>,
    next_id: IslandId,
    status: String,
}

type IslandId = u64;

struct OpenIsland {
    id: IslandId,
    demo: Demo,
    island: Island,
}

#[derive(Clone, Debug)]
enum Message {
    Open(Demo),
    Select(IslandId),
    Island(IslandId, kettle_ui_iced::Message),
}

impl Shell {
    fn new() -> (Self, Task<Message>) {
        let mut shell = Shell {
            islands: Vec::new(),
            active: None,
            next_id: 0,
            status: String::new(),
        };
        let task = shell.open(Demo::Contact);
        (shell, task)
    }

    fn open(&mut self, demo: Demo) -> Task<Message> {
        let (island, task) = match Island::new(demo.builder()) {
            Ok(built) => built,
            Err(e) => {
                self.status = format!("Couldn't open {}: {e}", demo.title());
                eprintln!("hob: {}", self.status);
                return Task::none();
            }
        };
        let id = self.next_id;
        self.next_id += 1;
        self.islands.push(OpenIsland { id, demo, island });
        self.active = Some(id);
        task.map(move |m| Message::Island(id, m))
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Open(demo) => self.open(demo),
            Message::Select(id) => {
                self.active = Some(id);
                Task::none()
            }
            Message::Island(id, message) => {
                // Messages can still arrive for an island that was just closed.
                let Some(index) = self.islands.iter().position(|i| i.id == id) else {
                    return Task::none();
                };
                let open = &mut self.islands[index];
                let (task, outcome) = open.island.update(message);
                if let Some(outcome) = outcome {
                    let title = open.demo.title();
                    self.status = match outcome {
                        Outcome::Submitted(fields) => {
                            format!("{title}: {}", open.demo.summary(&fields))
                        }
                        Outcome::Cancelled => format!("{title}: cancelled"),
                    };
                    println!("{}", self.status);
                    self.close(index);
                }
                task.map(move |m| Message::Island(id, m))
            }
        }
    }

    fn close(&mut self, index: usize) {
        let closed = self.islands.remove(index);
        if self.active == Some(closed.id) {
            let next = self.islands.get(index).or(self.islands.last());
            self.active = next.map(|i| i.id);
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let toolbar = row(Demo::ALL.iter().map(|&demo| {
            button(text(format!("New {}", demo.title().to_lowercase())))
                .style(button::secondary)
                .on_press(Message::Open(demo))
                .into()
        }))
        .spacing(spacing());

        let tabs = row(self.islands.iter().map(|open| {
            let style = if self.active == Some(open.id) {
                button::primary
            } else {
                button::text
            };
            button(text(open.demo.title()))
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
            column([toolbar.into(), tabs.into(), text(&self.status).into()])
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
