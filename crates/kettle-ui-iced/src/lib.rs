//! The iced backend for kettle-ui.
//!
//! An [`Island`] is embedded in a host iced application, which drives it
//! through [`Island::update`] and [`Island::view`]. A frozen island is shown
//! with [`view_frozen`].
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
//! widget's internal state (focus, cursor, scroll) across rebuilds. Widgets
//! with nothing to show (see [`is_visible`]) get no gaps around them, but
//! stay in the iced tree for the same reason.
//!
//! [`render_png`] renders an island offscreen, without a window; the
//! `kettle-render` binary does the same for an island saved as JSON.

mod offscreen;

use std::sync::Arc;

use iced::widget::{
    Container, button, checkbox, column, container, progress_bar, row, text, text_input,
};
use iced::{Element, Padding, Task, task};
use kettle_ui::widget::{Action, Kind, Text, Widget, is_shown, is_visible};
use kettle_ui::{
    Built, Contents, Error, FrozenIsland, IslandBuilder, Job, JobResult, Model, Outcome,
    StateUpdate, Value,
};

pub use offscreen::render_png;

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
        let jobs = match message {
            Msg::TextChanged(id, value) => self.model.set_text(&id, value),
            Msg::Toggled(id, value) => self.model.set_checked(&id, value),
            Msg::StateSet(update) => self.model.apply(update),
            Msg::Derived(result) => self.model.finish(result),
            Msg::Submit => {
                let fields = self.model.fields().clone();
                return (Task::none(), Some(Outcome::Submitted(fields)));
            }
            Msg::Cancel => return (Task::none(), Some(Outcome::Cancelled)),
        };
        (run(jobs), None)
    }

    /// The island's UI. It doesn't scroll or add padding around itself; that's
    /// up to the host.
    pub fn view(&self) -> Element<'_, Message> {
        render(&self.model, self.model.root(), true)
    }

    /// A snapshot of the island that can't change (see [`Model::freeze`]).
    pub fn freeze(&self) -> FrozenIsland {
        self.model.freeze()
    }
}

/// Shows a frozen island: like [`Island::view`], but with all interaction
/// disabled. It sends no messages, so it fits any message type.
pub fn view_frozen<'a, M: 'a>(island: &'a FrozenIsland) -> Element<'a, M> {
    render(island, island.root(), false).map(|_| unreachable!("frozen islands send no messages"))
}

/// Renders `widget` from `contents`. Without `interactive`, no widget sends
/// messages, so they all look disabled.
fn render<'a>(
    contents: &'a impl Contents,
    widget: &'a Widget,
    interactive: bool,
) -> Element<'a, Message> {
    let fields = contents.fields();
    let send = |msg: Msg| interactive.then_some(Message(msg));
    match widget.kind() {
        Kind::Column(children) => column(spaced(contents, children, interactive, |g| {
            Padding::ZERO.top(g)
        }))
        .into(),
        Kind::Row(children) => row(spaced(contents, children, interactive, |g| {
            Padding::ZERO.left(g)
        }))
        .into(),
        Kind::When { state, child } => {
            // Disabled while hidden, so a focused text field stops taking input.
            let shown = is_shown(contents, state);
            hideable(render(contents, child, interactive && shown), shown).into()
        }
        Kind::Label(Text::Static(s)) => text(s).into(),
        Kind::Label(Text::State(name)) => text(
            contents
                .state(name)
                .map(Value::to_string)
                .unwrap_or_default(),
        )
        .into(),
        Kind::TextField { id, placeholder } => text_input(placeholder, fields.text(id))
            .on_input_maybe(interactive.then_some(|s| Message(Msg::TextChanged(id.clone(), s))))
            .into(),
        Kind::Checkbox { id, label } => checkbox(fields.checked(id))
            .label(label)
            .on_toggle_maybe(interactive.then_some(|b| Message(Msg::Toggled(id.clone(), b))))
            .into(),
        Kind::Progress { state } => {
            let value = match contents.state(state) {
                Some(Value::Number(n)) => n.clamp(0.0, 1.0) as f32,
                _ => 0.0,
            };
            progress_bar(0.0..=1.0, value).into()
        }
        Kind::List { state } => column(list(contents, state).iter().map(|s| text(s).into()))
            .spacing(2)
            .into(),
        Kind::Suggestions { state, field } => column(list(contents, state).iter().map(|s| {
            button(text(s))
                .style(button::text)
                .padding([2, 0])
                .on_press_maybe(send(Msg::TextChanged(field.clone(), s.clone())))
                .into()
        }))
        .into(),
        Kind::Button { label, action } => button(text(label))
            .on_press_maybe(send(match action {
                Action::Submit => Msg::Submit,
                Action::Cancel => Msg::Cancel,
            }))
            .into(),
    }
}

/// Renders the children of a column or row, with a leading gap (from `gap`)
/// before each visible child except the first. Hidden children (see
/// [`is_visible`]) get no gap and zero size, but stay in place: iced matches
/// widget state to children by position. Every child is wrapped the same
/// way, so the tree's shape doesn't depend on visibility.
fn spaced<'a>(
    contents: &'a impl Contents,
    children: &'a [Widget],
    interactive: bool,
    gap: impl Fn(f32) -> Padding,
) -> impl Iterator<Item = Element<'a, Message>> {
    let mut first = true;
    children.iter().map(move |child| {
        let visible = is_visible(contents, child);
        let padding = if visible && !first {
            gap(spacing())
        } else {
            Padding::ZERO
        };
        first &= !visible;
        hideable(render(contents, child, interactive), visible)
            .padding(padding)
            .into()
    })
}

/// Wraps `content` in a container that shrinks it to nothing unless
/// `visible`. Always the same wrapper, so the tree's shape doesn't depend on
/// visibility.
fn hideable(content: Element<'_, Message>, visible: bool) -> Container<'_, Message> {
    let max = if visible { f32::INFINITY } else { 0.0 };
    container(content)
        .max_width(max)
        .max_height(max)
        .clip(!visible)
}

/// Named state as a list; empty if it's not a list (or not computed yet).
fn list<'a>(contents: &'a impl Contents, name: &str) -> &'a [String] {
    match contents.state(name) {
        Some(Value::List(items)) => items,
        _ => &[],
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
