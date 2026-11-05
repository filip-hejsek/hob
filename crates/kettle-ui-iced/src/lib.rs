//! The iced backend for kettle-ui.
//!
//! An [`Island`] is embedded in a host iced application, which drives it
//! through [`Island::update`] and [`Island::view`].
//!
//! ```
//! use kettle_ui::IslandBuilder;
//! use kettle_ui::widget::*;
//! use kettle_ui_iced::Island;
//!
//! let mut builder = IslandBuilder::new(column([
//!     text_field("name", "Your name"),
//!     label(state("greeting")),
//!     submit_button("OK"),
//! ]));
//! builder.derive("greeting", |f| format!("Hello, {}!", f.text("name")).into());
//! // The host keeps the island, and runs the task (mapped to its own
//! // message type).
//! let (island, task) = Island::new(builder).unwrap();
//! ```
//!
//! The host calls `view` after every update, which rebuilds iced widgets from
//! the kettle-ui tree. Because the tree never changes shape, iced keeps each
//! widget's internal state (focus, cursor, scroll) across rebuilds.

use std::sync::Arc;

use iced::widget::{button, checkbox, column, progress_bar, row, text, text_input};
use iced::{Element, Task, task};
use kettle_ui::widget::{Action, Kind, Text, Widget};
use kettle_ui::{Built, Error, IslandBuilder, Job, JobResult, Model, Outcome, StateUpdate, Value};

/// The gap between neighbouring widgets in a [`column`](kettle_ui::widget::column)
/// or [`row`](kettle_ui::widget::row), in logical pixels. Hosts should use it too, so
/// their UI around an island matches it.
///
/// Fixed for now; a function so it can become configurable later.
pub fn spacing() -> f32 {
    10.0
}

/// A kettle-ui island in an iced application. The host shows
/// [`Island::view`], passes the resulting messages to [`Island::update`], and
/// runs the tasks it gets back.
pub struct Island {
    model: Model,
    /// Stops delivering [`StateHandle`](kettle_ui::StateHandle) updates when
    /// the island is dropped.
    _updates: task::Handle,
}

/// A message for an [`Island`]. The host wraps these in its own message type.
#[derive(Clone, Debug)]
pub struct Message(Msg);

#[derive(Clone, Debug)]
enum Msg {
    TextChanged(Arc<str>, String),
    Toggled(Arc<str>, bool),
    StateSet(StateUpdate),
    Derived(JobResult),
    Submit,
    Cancel,
}

impl Island {
    /// Checks the tree and creates the island (see
    /// [`IslandBuilder::build`]). The host must run the returned task (mapped
    /// to its own message type) for the island to work: it delivers
    /// [`StateHandle`](kettle_ui::StateHandle) updates and the first derived
    /// values.
    pub fn new(builder: IslandBuilder) -> Result<(Self, Task<Message>), Error> {
        let Built {
            model,
            jobs,
            updates,
        } = builder.build()?;
        let (updates, handle) = Task::run(updates, |u| Message(Msg::StateSet(u))).abortable();
        let island = Island {
            model,
            _updates: handle.abort_on_drop(),
        };
        Ok((island, Task::batch([updates, run(jobs)])))
    }

    /// Handles a message from this island's [`view`](Island::view) or tasks.
    /// Returns a task for the host to run, and an outcome if the user just
    /// submitted or cancelled.
    pub fn update(&mut self, Message(message): Message) -> (Task<Message>, Option<Outcome>) {
        match message {
            Msg::TextChanged(id, value) => return (run(self.model.set_text(&id, value)), None),
            Msg::Toggled(id, value) => return (run(self.model.set_checked(&id, value)), None),
            Msg::StateSet(update) => self.model.apply(update),
            Msg::Derived(result) => self.model.finish(result),
            Msg::Submit => {
                let fields = self.model.fields().clone();
                return (Task::none(), Some(Outcome::Submitted(fields)));
            }
            Msg::Cancel => return (Task::none(), Some(Outcome::Cancelled)),
        }
        (Task::none(), None)
    }

    /// The island's UI. It doesn't scroll or add padding around itself; that's
    /// up to the host.
    pub fn view(&self) -> Element<'_, Message> {
        self.render(self.model.root())
    }

    fn render<'a>(&'a self, widget: &'a Widget) -> Element<'a, Message> {
        let fields = self.model.fields();
        match widget.kind() {
            Kind::Column(children) => column(children.iter().map(|c| self.render(c)))
                .spacing(spacing())
                .into(),
            Kind::Row(children) => row(children.iter().map(|c| self.render(c)))
                .spacing(spacing())
                .into(),
            Kind::Label(Text::Static(s)) => text(s).into(),
            Kind::Label(Text::State(name)) => text(
                self.model
                    .state(name)
                    .map(Value::to_string)
                    .unwrap_or_default(),
            )
            .into(),
            Kind::TextField { id, placeholder } => text_input(placeholder, fields.text(id))
                .on_input(|s| Message(Msg::TextChanged(id.clone(), s)))
                .into(),
            Kind::Checkbox { id, label } => checkbox(fields.checked(id))
                .label(label)
                .on_toggle(|b| Message(Msg::Toggled(id.clone(), b)))
                .into(),
            Kind::Progress { state } => {
                let value = match self.model.state(state) {
                    Some(Value::Number(n)) => n.clamp(0.0, 1.0) as f32,
                    _ => 0.0,
                };
                progress_bar(0.0..=1.0, value).into()
            }
            Kind::List { state } => column(self.list(state).iter().map(|s| text(s).into()))
                .spacing(2)
                .into(),
            Kind::Suggestions { state, field } => column(self.list(state).iter().map(|s| {
                button(text(s))
                    .style(button::text)
                    .padding([2, 0])
                    .on_press(Message(Msg::TextChanged(field.clone(), s.clone())))
                    .into()
            }))
            .into(),
            Kind::Button { label, action } => button(text(label))
                .on_press(Message(match action {
                    Action::Submit => Msg::Submit,
                    Action::Cancel => Msg::Cancel,
                }))
                .into(),
        }
    }

    /// Named state as a list; empty if it's not a list (or not computed yet).
    fn list(&self, name: &str) -> &[String] {
        match self.model.state(name) {
            Some(Value::List(items)) => items,
            _ => &[],
        }
    }
}

/// Runs jobs that compute derived state.
fn run(jobs: Vec<Job>) -> Task<Message> {
    Task::batch(jobs.into_iter().map(|job| {
        // The default executor is a thread pool, so blocking here is fine.
        Task::perform(async move { job.run() }, |result| {
            Message(Msg::Derived(result))
        })
    }))
}
