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
//! The text fields of a live island have iced [`Id`]s, made when the island
//! is created and kept for its whole life, for the host's focus operations
//! (see [`Island::field_id`] and [`Island::has_focus`]). They are unique, so
//! they never clash with the host's own Ids or another island's. An island
//! without text fields can have focus as a whole, with its own Id;
//! [`Island::focus`] focuses an island either way.
//!
//! A live island handles some keys itself, while it has focus: Enter in a
//! text field focuses the next shown one, and submits in the last one (if
//! there's a submit button); Escape cancels (if there's a cancel button);
//! and a button's key (see [`ButtonLabel`](kettle_ui::widget::ButtonLabel))
//! presses it.
//!
//! [`render_png`] renders an island offscreen, without a window; the
//! `kettle-render` binary does the same for an island saved as JSON.

mod offscreen;
mod scope;

use std::sync::Arc;

use iced::advanced::widget::operation::{Focusable, Outcome as OperationOutcome};
use iced::advanced::widget::{Id, Operation, operate};
use iced::widget::{
    Container, button, checkbox, column, container, operation, progress_bar, row, text, text_input,
};
use iced::{Element, Padding, Rectangle, Task, task};
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
    /// The iced Ids of the text fields, in tree order.
    fields: Vec<(Arc<str>, Id)>,
    /// The Id of the island as a whole, which can have focus if it has no
    /// text fields.
    id: Id,
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
        let mut fields = Vec::new();
        text_fields(model.root(), &mut fields);
        let island = Island {
            model,
            fields: fields.into_iter().map(|f| (f, Id::unique())).collect(),
            id: Id::unique(),
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
        let mut shown = Shown::default();
        shown.collect(&self.model, self.model.root(), &self.fields);
        let message = |action| match action {
            Action::Submit => Message(Msg::Submit),
            Action::Cancel => Message(Msg::Cancel),
        };
        scope::Scope {
            content: render(&self.model, self.model.root(), true, Some(&self.fields)),
            id: self.id.clone(),
            focusable: self.fields.is_empty(),
            fields: shown.fields,
            keys: (shown.keys.into_iter())
                .map(|(key, action)| (key, message(action)))
                .collect(),
            submit: shown.submit.then(|| message(Action::Submit)),
            cancel: shown.cancel.then(|| message(Action::Cancel)),
        }
        .into()
    }

    /// Focuses the island: its first shown text field, or the island as a
    /// whole if it has no text fields. Does nothing if all its text fields
    /// are hidden.
    pub fn focus<T: Send + 'static>(&self) -> Task<T> {
        if self.fields.is_empty() {
            return operation::focus(self.id.clone());
        }
        let mut shown = Shown::default();
        shown.collect(&self.model, self.model.root(), &self.fields);
        match shown.fields.into_iter().next() {
            Some(id) => operation::focus(id),
            None => Task::none(),
        }
    }

    /// The iced Id of text field `field`, for focus operations. `None` if the
    /// island has no text field with this id.
    pub fn field_id(&self, field: &str) -> Option<Id> {
        field_id(&self.fields, field).cloned()
    }

    /// Whether the island or one of its text fields has focus.
    pub fn has_focus(&self) -> Task<bool> {
        let fields = self.fields.iter().map(|(_, id)| id.clone());
        let ids = fields.chain([self.id.clone()]).collect();
        operate(HasFocus {
            ids,
            focused: false,
        })
    }

    /// A snapshot of the island that can't change (see [`Model::freeze`]).
    pub fn freeze(&self) -> FrozenIsland {
        self.model.freeze()
    }
}

/// Shows a frozen island: like [`Island::view`], but with all interaction
/// disabled. It sends no messages, so it fits any message type.
pub fn view_frozen<'a, M: 'a>(island: &'a FrozenIsland) -> Element<'a, M> {
    render(island, island.root(), false, None)
        .map(|_| unreachable!("frozen islands send no messages"))
}

/// The iced Ids of text fields, in tree order.
type FieldIds = [(Arc<str>, Id)];

/// Renders `widget` from `contents`. Without `interactive`, no widget sends
/// messages, so they all look disabled. Text fields get their Ids from `ids`.
fn render<'a>(
    contents: &'a impl Contents,
    widget: &'a Widget,
    interactive: bool,
    ids: Option<&'a FieldIds>,
) -> Element<'a, Message> {
    let fields = contents.fields();
    let send = |msg: Msg| interactive.then_some(Message(msg));
    match widget.kind() {
        Kind::Column(children) => column(spaced(contents, children, interactive, ids, |g| {
            Padding::ZERO.top(g)
        }))
        .into(),
        Kind::Row(children) => row(spaced(contents, children, interactive, ids, |g| {
            Padding::ZERO.left(g)
        }))
        .into(),
        Kind::When { state, child } => {
            // Disabled while hidden, so a focused text field stops taking input.
            let shown = is_shown(contents, state);
            hideable(render(contents, child, interactive && shown, ids), shown).into()
        }
        Kind::Label(Text::Static(s)) => text(s).into(),
        Kind::Label(Text::State(name)) => text(
            contents
                .state(name)
                .map(Value::to_string)
                .unwrap_or_default(),
        )
        .into(),
        Kind::TextField { id, placeholder } => {
            let mut input = text_input(placeholder, fields.text(id)).on_input_maybe(
                interactive.then_some(|s| Message(Msg::TextChanged(id.clone(), s))),
            );
            if let Some(widget_id) = ids.and_then(|ids| field_id(ids, id)) {
                input = input.id(widget_id.clone());
            }
            input.into()
        }
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
        Kind::Button { label, key, action } => button(text(match key {
            Some(key) if label.is_empty() => format!("[{key}]"),
            Some(key) => format!("{label} [{key}]"),
            None => label.clone(),
        }))
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
    ids: Option<&'a FieldIds>,
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
        hideable(render(contents, child, interactive, ids), visible)
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

/// What a live island shows that its keys act on.
#[derive(Default)]
struct Shown {
    /// The Ids of shown text fields, in tree order.
    fields: Vec<Id>,
    /// The keys of shown buttons.
    keys: Vec<(char, Action)>,
    /// Whether a submit button is shown.
    submit: bool,
    /// Whether a cancel button is shown.
    cancel: bool,
}

impl Shown {
    /// Collects what's shown of `widget`; text fields get their Ids from
    /// `ids`.
    fn collect(&mut self, contents: &impl Contents, widget: &Widget, ids: &FieldIds) {
        match widget.kind() {
            Kind::Column(children) | Kind::Row(children) => {
                children.iter().for_each(|c| self.collect(contents, c, ids));
            }
            Kind::When { state, child } if is_shown(contents, state) => {
                self.collect(contents, child, ids);
            }
            Kind::TextField { id, .. } => self.fields.extend(field_id(ids, id).cloned()),
            Kind::Button { key, action, .. } => {
                match action {
                    Action::Submit => self.submit = true,
                    Action::Cancel => self.cancel = true,
                }
                if let Some(key) = key {
                    self.keys.push((*key, *action));
                }
            }
            _ => {}
        }
    }
}

/// Collects the ids of the text fields in `widget`, in tree order.
fn text_fields(widget: &Widget, out: &mut Vec<Arc<str>>) {
    match widget.kind() {
        Kind::Column(children) | Kind::Row(children) => {
            children.iter().for_each(|c| text_fields(c, out));
        }
        Kind::When { child, .. } => text_fields(child, out),
        Kind::TextField { id, .. } => out.push(id.clone()),
        _ => {}
    }
}

/// The Id of text field `field`.
fn field_id<'a>(ids: &'a FieldIds, field: &str) -> Option<&'a Id> {
    ids.iter().find(|(f, _)| **f == *field).map(|(_, id)| id)
}

/// Finds out whether one of `ids` has focus. Unlike iced's `find_focused`, it
/// always gives an answer.
struct HasFocus {
    ids: Vec<Id>,
    focused: bool,
}

impl Operation<bool> for HasFocus {
    fn focusable(&mut self, id: Option<&Id>, _: Rectangle, state: &mut dyn Focusable) {
        if state.is_focused() && id.is_some_and(|id| self.ids.contains(id)) {
            self.focused = true;
        }
    }

    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<bool>)) {
        operate(self);
    }

    fn finish(&self) -> OperationOutcome<bool> {
        OperationOutcome::Some(self.focused)
    }
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

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::{Duration, Instant};

    use futures_channel::mpsc;
    use iced::Size;
    use iced::mouse;
    use iced::widget::operation;
    use iced_test::emulator::{Emulator, Event, Mode};
    use iced_test::instruction::{Expectation, Interaction, Key, Keyboard, Mouse, Target};
    use iced_test::{Instruction, simulator};
    use kettle_ui::widget::{
        cancel_button, checkbox, column, row, submit_button, text_field, when,
    };

    use super::*;

    fn island() -> Island {
        let mut builder = IslandBuilder::new(column([
            text_field("a", "A"),
            row([checkbox("c", "C"), when("show", text_field("b", "B"))]),
        ]));
        builder.set_state("show", true);
        Island::new(builder).unwrap().0
    }

    #[test]
    fn text_fields_have_ids_in_tree_order() {
        let island = island();
        let fields: Vec<&str> = island.fields.iter().map(|(f, _)| &**f).collect();
        assert_eq!(fields, ["a", "b"]);
        assert_eq!(island.field_id("c"), None);
        assert_eq!(island.field_id("x"), None);
        assert_ne!(island.field_id("a"), island.field_id("b"));
    }

    #[test]
    fn ids_differ_between_islands() {
        assert_ne!(island().field_id("a"), island().field_id("a"));
    }

    #[test]
    fn field_id_finds_the_field() {
        let mut island = island();
        let id = island.field_id("b").unwrap();
        let messages: Vec<_> = {
            let mut ui = simulator(island.view());
            ui.click(id).unwrap();
            ui.typewrite("x");
            ui.into_messages().collect()
        };
        for message in messages {
            let _ = island.update(message);
        }
        let frozen = island.freeze();
        assert_eq!(frozen.fields().text("a"), "");
        assert_eq!(frozen.fields().text("b"), "x");
    }

    /// An island, whether it had focus at the last check, and how the user
    /// finished with it, for [`program`].
    struct State {
        island: Island,
        focused: Option<bool>,
        outcome: Option<String>,
    }

    #[derive(Clone, Debug)]
    enum Msg {
        Island(super::Message),
        Focused(bool),
    }

    impl State {
        fn update(&mut self, message: Msg) -> Task<Msg> {
            match message {
                Msg::Island(m) => {
                    let (task, outcome) = self.island.update(m);
                    // Only the first.
                    if let Some(outcome) = outcome.filter(|_| self.outcome.is_none()) {
                        self.outcome = Some(match outcome {
                            Outcome::Submitted(fields) => {
                                format!("submitted {:?} {:?}", fields.text("a"), fields.text("b"))
                            }
                            Outcome::Cancelled => "cancelled".into(),
                        });
                    }
                    task.map(Msg::Island)
                }
                Msg::Focused(focused) => {
                    self.focused = Some(focused);
                    Task::none()
                }
            }
        }

        fn view(&self) -> Element<'_, Msg> {
            iced::widget::column![
                self.island.view().map(Msg::Island),
                text(format!("focused: {:?}", self.focused)),
                text(format!("outcome: {:?}", self.outcome)),
            ]
            .into()
        }
    }

    /// What [`program`] focuses on start.
    #[derive(Clone, Copy)]
    enum Focus {
        Nothing,
        Field(&'static str),
        /// With [`Island::focus`].
        Island,
    }

    /// An island from `make` as an iced program, which checks on start
    /// whether the island has focus, after focusing what `focus` says.
    fn program(make: fn() -> Island, focus: Focus) -> iced::Application<impl iced::Program> {
        let boot = move || {
            let island = make();
            let focus = match focus {
                Focus::Nothing => Task::none(),
                Focus::Field(field) => operation::focus(island.field_id(field).unwrap()),
                Focus::Island => island.focus(),
            };
            let check = focus.chain(island.has_focus().map(Msg::Focused));
            let state = State {
                island,
                focused: None,
                outcome: None,
            };
            (state, check)
        };
        iced::application(boot, State::update, State::view)
    }

    /// Runs `program`, then each instruction, and checks that they succeed
    /// within a few seconds (the emulator doesn't wait for tasks).
    fn check<P: iced_test::program::Program + 'static>(program: P, steps: &[Instruction]) {
        let (sender, mut events) = mpsc::channel(64);
        let mut emulator =
            Emulator::new(sender, &program, Mode::Immediate, Size::new(400.0, 300.0));
        let start = Instant::now();
        let mut steps = steps.iter();
        let mut current = None::<&Instruction>;
        loop {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "timed out at {current:?}"
            );
            match events.try_recv() {
                Ok(Event::Action(action)) => emulator.perform(&program, action),
                Ok(Event::Ready) => match steps.next() {
                    Some(step) => {
                        current = Some(step);
                        emulator.run(&program, step.clone());
                    }
                    None => return,
                },
                // Retry: tasks may not have run yet.
                Ok(Event::Failed(step)) => {
                    thread::sleep(Duration::from_millis(5));
                    emulator.run(&program, step);
                }
                Err(mpsc::TryRecvError::Empty) => thread::sleep(Duration::from_millis(1)),
                Err(mpsc::TryRecvError::Closed) => panic!("the emulator stopped"),
            }
        }
    }

    fn expect(text: &str) -> Instruction {
        Instruction::Expect(Expectation::Text(text.into()))
    }

    #[test]
    fn has_focus_is_false_without_focus() {
        check(
            program(island, Focus::Nothing),
            &[expect("focused: Some(false)")],
        );
    }

    #[test]
    fn has_focus_is_true_after_focusing_a_field() {
        let typewrite =
            Instruction::Interact(Interaction::Keyboard(Keyboard::Typewrite("typed".into())));
        check(
            program(island, Focus::Field("b")),
            &[expect("focused: Some(true)"), typewrite, expect("typed")],
        );
    }

    fn typewrite(text: &str) -> Instruction {
        Instruction::Interact(Interaction::Keyboard(Keyboard::Typewrite(text.into())))
    }

    fn press(key: Key) -> Instruction {
        Instruction::Interact(Interaction::Keyboard(Keyboard::Type(key)))
    }

    /// Two text fields with a hidden one between them, and both buttons.
    fn form() -> Island {
        let mut builder = IslandBuilder::new(column([
            text_field("a", "A"),
            when("hidden", text_field("h", "H")),
            text_field("b", "B"),
            row([submit_button("OK"), cancel_button("Cancel")]),
        ]));
        builder.set_state("hidden", false);
        Island::new(builder).unwrap().0
    }

    /// Buttons with keys, and no text fields.
    fn question() -> Island {
        let builder = IslandBuilder::new(row([submit_button("Yes [y]"), cancel_button("No [n]")]));
        Island::new(builder).unwrap().0
    }

    #[test]
    fn island_focus_goes_to_the_first_field() {
        check(
            program(form, Focus::Island),
            &[expect("focused: Some(true)"), typewrite("x"), expect("x")],
        );
    }

    #[test]
    fn enter_goes_to_the_next_shown_field_then_submits() {
        check(
            program(form, Focus::Field("a")),
            &[
                expect("focused: Some(true)"),
                typewrite("1"),
                press(Key::Enter),
                typewrite("2"),
                press(Key::Enter),
                expect(r#"outcome: Some("submitted \"1\" \"2\"")"#),
            ],
        );
    }

    #[test]
    fn escape_cancels() {
        check(
            program(form, Focus::Field("b")),
            &[
                expect("focused: Some(true)"),
                press(Key::Escape),
                expect(r#"outcome: Some("cancelled")"#),
            ],
        );
    }

    #[test]
    fn keys_press_buttons_of_a_focused_island() {
        check(
            program(question, Focus::Island),
            &[
                expect("focused: Some(true)"),
                expect("No [n]"),
                typewrite("n"),
                expect(r#"outcome: Some("cancelled")"#),
            ],
        );
    }

    #[test]
    fn keys_do_nothing_without_focus() {
        check(
            program(question, Focus::Nothing),
            &[
                expect("focused: Some(false)"),
                typewrite("y"),
                // Only the first outcome is kept, so this checks that "y"
                // did nothing.
                Instruction::Interact(Interaction::Mouse(Mouse::Click {
                    button: mouse::Button::Left,
                    target: Some(Target::Text("No [n]".into())),
                })),
                expect(r#"outcome: Some("cancelled")"#),
            ],
        );
    }
}
