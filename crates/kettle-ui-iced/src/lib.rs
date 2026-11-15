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
//! In this crate, "text fields" include text areas, unless said otherwise,
//! and "fields" are text fields, checkboxes and the options of radio
//! groups: what can have focus. Checkboxes next to each other form a run,
//! and so do the options of a radio group.
//! The fields of a live island have iced [`Id`]s, made when the island
//! is created and kept for its whole life, for the host's focus operations
//! (see [`Island::field_id`] and [`Island::has_focus`]). They are unique, so
//! they never clash with the host's own Ids or another island's. An island
//! can also have focus as a whole, with its own Id: one without fields when
//! clicked, one with fields only when focus moves on from its last field;
//! [`Island::focus`] focuses an island either way.
//!
//! A live island handles some keys itself, while it has focus: Enter in a
//! field focuses the next shown one, and submits in the last one (if
//! there's a submit button), while in a text area it starts a new line and
//! Ctrl+Enter does that instead. In a run, Enter skips the rest of it, to
//! the next other field or else submitting; it never selects a radio
//! option. In a checkbox, Space toggles it, `y` and `n` check and uncheck
//! it and move on (after the last field, to the island as a whole; they
//! never submit); a border and a muted `[y/n]` after its label show it has
//! focus. In a radio option, Space selects it, and an option's number or
//! letter selects that option and focuses it; while one of a group's
//! options has focus, all of them show their key (the letter, or else the
//! number). In a field other than a text area, Up and Down move between
//! fields, and Down from the last one to the island as a whole; Ctrl+Up
//! and Ctrl+Down do that in a text area too, and in a run skip the rest of
//! it; landing on a run, they go to its selected radio option, or else the
//! field there that last had focus. Escape
//! cancels (if there's a cancel button). While the island as a whole has focus,
//! Enter submits and a button's key (see
//! [`ButtonKey`]) presses it; a key with Alt
//! presses it also while one of the island's fields has focus, and
//! Alt+Enter submits from any field.
//!
//! [`render_png`] renders an island offscreen, without a window; the
//! `kettle-render` binary does the same for an island saved as JSON.

mod choice;
mod grid;
mod offscreen;
mod scope;
mod text_area;

use std::sync::Arc;

use iced::advanced::widget::operation::{Focusable, Outcome as OperationOutcome};
use iced::advanced::widget::{Id, Operation, operate};
use iced::widget::{
    Container, button, column, container, operation, progress_bar, rule, text, text_input,
};
use iced::{Element, Padding, Rectangle, Task, task};
use kettle_ui::widget::{
    Action, Borders, ButtonKey, Kind, Text, Widget, is_shown, is_visible, option_digit,
};
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
    /// The iced Ids of the fields, in tree order.
    fields: Vec<(Arc<str>, Id)>,
    /// The Id of the island as a whole, which can also have focus.
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
    /// A radio group and the value of the option selected.
    Selected(Arc<str>, Arc<str>),
    StateSet(StateUpdate),
    Derived(JobResult),
    Submit,
    Cancel,
    Press(Arc<str>),
    Run(Arc<str>),
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
        field_ids(model.root(), &mut fields);
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
    /// finished with the island. A submit has its outcome once the validators
    /// pass ([`Model::submit`]), which may be after a later message.
    pub fn update(&mut self, Message(message): Message) -> (Task<Message>, Option<Outcome>) {
        let jobs = match message {
            Msg::TextChanged(id, value) => self.model.set_text(&id, value),
            Msg::Toggled(id, value) => self.model.set_checked(&id, value),
            Msg::Selected(id, value) => self.model.set_selected(&id, &value),
            Msg::StateSet(update) => self.model.apply(update),
            Msg::Derived(result) => self.model.finish(result),
            // Submitted below, once the validators pass.
            Msg::Submit => self.model.submit(),
            Msg::Run(id) => self.model.run_action(&id),
            Msg::Cancel => return (Task::none(), Some(Outcome::Cancelled)),
            Msg::Press(id) => {
                let fields = self.model.fields().clone();
                return (Task::none(), Some(Outcome::Pressed(id, fields)));
            }
        };
        let submitted = self.model.take_submitted().map(Outcome::Submitted);
        (run(jobs), submitted)
    }

    /// The island's UI. It doesn't scroll or add padding around itself; that's
    /// up to the host.
    pub fn view(&self) -> Element<'_, Message> {
        let mut shown = Shown::default();
        shown.collect(&self.model, self.model.root(), &self.fields);
        let message = |action| Message(action_msg(action));
        scope::Scope {
            content: render(&self.model, self.model.root(), true, Some(&self.fields)),
            id: self.id.clone(),
            click_focus: self.fields.is_empty(),
            fields: shown.fields,
            keys: (shown.keys.into_iter())
                .map(|(key, action)| (key, message(action)))
                .collect(),
            submit: shown.submit.then(|| message(Action::Submit)),
            cancel: shown.cancel.then(|| message(Action::Cancel)),
        }
        .into()
    }

    /// Focuses the island: its first shown field (in a radio group, its
    /// selected option), or the island as a whole if it has no fields. Does
    /// nothing if all its fields are hidden.
    pub fn focus<T: Send + 'static>(&self) -> Task<T> {
        if self.fields.is_empty() {
            return operation::focus(self.id.clone());
        }
        let mut shown = Shown::default();
        shown.collect(&self.model, self.model.root(), &self.fields);
        if shown.fields.is_empty() {
            return Task::none();
        }
        let first = &shown.fields[scope::run(&shown.fields, 0)];
        let field = match first[0].kind {
            scope::FieldKind::RadioOption { .. } => scope::selected_option(first),
            _ => None,
        };
        operation::focus(field.unwrap_or(&first[0]).id.clone())
    }

    /// The iced Id of the island as a whole; operations see its bounds as
    /// those of a focusable widget with this Id.
    pub fn id(&self) -> Id {
        self.id.clone()
    }

    /// The iced Id of field `field` (a text field, checkbox or radio group),
    /// for focus operations: for a radio group, the Id of its selected
    /// option, or else its first. `None` if the island has no field with this
    /// id.
    pub fn field_id(&self, field: &str) -> Option<Id> {
        let selected = self.model.fields().selected(field);
        let selected = selected.and_then(|value| option_index(self.model.root(), field, value));
        let mut ids = option_ids(&self.fields, field);
        match selected {
            Some(i) => ids.nth(i),
            None => ids.next(),
        }
        .cloned()
    }

    /// Whether the island or one of its fields has focus.
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

/// The iced Ids of fields, in tree order.
type FieldIds = [(Arc<str>, Id)];

/// Renders `widget` from `contents`. Without `interactive`, no widget sends
/// messages, so they all look disabled. Fields get their Ids from `ids`.
fn render<'a>(
    contents: &'a impl Contents,
    widget: &'a Widget,
    interactive: bool,
    ids: Option<&'a FieldIds>,
) -> Element<'a, Message> {
    let fields = contents.fields();
    let send = |msg: Msg| interactive.then_some(Message(msg));
    match widget.kind() {
        Kind::Column(children) => column(spaced(contents, children, interactive, ids)).into(),
        Kind::Row(children) => {
            // The grid puts the gaps in, and starts new lines.
            let visible = (children.iter()).map(|c| is_visible(contents, c)).collect();
            let children = (children.iter())
                .zip(&visible)
                .map(|(child, &visible)| {
                    hideable(render(contents, child, interactive, ids), visible).into()
                })
                .collect();
            grid::Grid::row(children, visible).into()
        }
        Kind::Table { rows, borders } => table(contents, rows, *borders, interactive, ids),
        Kind::When { state, child } => {
            // Disabled while hidden, so a focused text field stops taking input.
            let shown = is_shown(contents, state);
            hideable(render(contents, child, interactive && shown, ids), shown).into()
        }
        Kind::Label(Text::Static(s)) => wrapped(s.as_str()).into(),
        Kind::Label(Text::State(name)) => wrapped(
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
        Kind::TextArea { id, placeholder } => {
            let mut area = text_area::TextArea::new(fields.text(id), placeholder).on_change_maybe(
                interactive.then_some(|s| Message(Msg::TextChanged(id.clone(), s))),
            );
            if let Some(widget_id) = ids.and_then(|ids| field_id(ids, id)) {
                area = area.id(widget_id.clone());
            }
            area.into()
        }
        Kind::Checkbox { id, label } => {
            let mut checkbox = choice::Choice::checkbox(
                label,
                fields.checked(id),
                interactive.then_some(|b| Message(Msg::Toggled(id.clone(), b))),
            );
            if let Some(widget_id) = ids.and_then(|ids| field_id(ids, id)) {
                checkbox = checkbox.id(widget_id.clone());
            }
            checkbox.into()
        }
        Kind::RadioGroup { id, options } => {
            let mut widget_ids = ids.map(|ids| option_ids(ids, id));
            let selected = fields.selected(id);
            let options = (options.iter().enumerate())
                .map(|(i, option)| {
                    let mut radio = choice::Choice::radio(
                        &option.label,
                        selected == Some(&*option.value),
                        Message(Msg::Selected(id.clone(), option.value.clone())),
                        option.shown_key(i),
                        interactive,
                    );
                    if let Some(widget_id) = widget_ids.as_mut().and_then(Iterator::next) {
                        radio = radio.id(widget_id.clone());
                    }
                    radio
                })
                .collect();
            choice::RadioGroup::new(options, spacing()).into()
        }
        Kind::Progress { state } => {
            let value = match contents.state(state) {
                Some(Value::Number(n)) => n.clamp(0.0, 1.0) as f32,
                _ => 0.0,
            };
            progress_bar(0.0..=1.0, value).into()
        }
        Kind::Image { state } => match contents.state(state) {
            Some(Value::Image(image)) => image_view(image),
            _ => column([]).into(),
        },
        Kind::List { state } => column(list(contents, state).iter().map(|s| wrapped(s).into()))
            .spacing(2)
            .into(),
        Kind::Suggestions { state, field } => column(list(contents, state).iter().map(|s| {
            button(wrapped(s))
                .style(button::text)
                .padding([2, 0])
                .on_press_maybe(send(Msg::TextChanged(field.clone(), s.clone())))
                .into()
        }))
        .into(),
        Kind::Button {
            label,
            key,
            action,
            danger,
        } => button(wrapped(match key {
            Some(key) if label.is_empty() => format!("[{key}]"),
            Some(key) => format!("{label} [{key}]"),
            None => label.clone(),
        }))
        .style(if *danger {
            button::danger
        } else {
            button::primary
        })
        .on_press_maybe(send(action_msg(action.clone())))
        .into(),
    }
}

/// Renders a table with [`grid::Grid`], which draws lines only between
/// cells, and adds the outer ones: a border around it, or lines above and
/// below it.
fn table<'a>(
    contents: &'a impl Contents,
    rows: &'a [Vec<Widget>],
    borders: Borders,
    interactive: bool,
    ids: Option<&'a FieldIds>,
) -> Element<'a, Message> {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    if columns == 0 {
        return column([]).into();
    }
    let cells: Vec<Element<'a, Message>> = (rows.iter())
        .flat_map(|row| {
            (0..columns).map(|col| match row.get(col) {
                Some(cell) => render(contents, cell, interactive, ids),
                None => text("").into(),
            })
        })
        .collect();
    let table = grid::Grid::table(cells, columns, borders == Borders::All);
    match borders {
        Borders::All => container(table)
            .style(|theme: &iced::Theme| container::Style {
                border: iced::Border {
                    color: theme.extended_palette().background.strong.color,
                    width: 1.0,
                    radius: 0.0.into(),
                },
                ..container::Style::default()
            })
            .into(),
        Borders::Horizontal => {
            // The rules fill the width; without this, so would the column.
            let width = iced::advanced::Widget::size(&table).width;
            column([
                rule::horizontal(1).into(),
                table.into(),
                rule::horizontal(1).into(),
            ])
            .width(width)
            .into()
        }
    }
}

/// Text that wraps at word boundaries, or anywhere in a word too long for
/// a line.
fn wrapped<'a>(s: impl text::IntoFragment<'a>) -> iced::widget::Text<'a> {
    text(s).wrapping(text::Wrapping::WordOrGlyph)
}

/// Shows an image at its natural size, or scaled down to fit. If it's
/// missing or can't be decoded, shows a notice and its alt text instead.
fn image_view<'a>(image: &kettle_ui::Image) -> Element<'a, Message> {
    // Decoded once per image, and the same handle reused, so the renderer
    // keeps its copy instead of uploading the image again on every view.
    let handle = image.backend_cache(|image| {
        let rgba = image.data()?.decode().ok()?;
        Some(iced::widget::image::Handle::from_rgba(
            rgba.width,
            rgba.height,
            rgba.pixels,
        ))
    });
    match handle {
        Some(Some(handle)) => iced::widget::image(handle.clone()).into(),
        _ => {
            let notice = match image.data() {
                Some(_) => "Image can't be shown",
                None => "Image not available",
            };
            let mut lines = vec![wrapped(notice).style(text::secondary).into()];
            if !image.alt().is_empty() {
                lines.push(wrapped(image.alt().to_owned()).into());
            }
            column(lines).spacing(2).into()
        }
    }
}

/// Renders the children of a column, with a gap above each visible child
/// except the first. Hidden children (see
/// [`is_visible`]) get no gap and zero size, but stay in place: iced matches
/// widget state to children by position. Every child is wrapped the same
/// way, so the tree's shape doesn't depend on visibility.
fn spaced<'a>(
    contents: &'a impl Contents,
    children: &'a [Widget],
    interactive: bool,
    ids: Option<&'a FieldIds>,
) -> impl Iterator<Item = Element<'a, Message>> {
    let mut first = true;
    children.iter().map(move |child| {
        let visible = is_visible(contents, child);
        let padding = if visible && !first {
            Padding::ZERO.top(spacing())
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
    /// The shown fields, in tree order.
    fields: Vec<scope::Field>,
    /// The keys of shown buttons.
    keys: Vec<(ButtonKey, Action)>,
    /// Whether a submit button is shown.
    submit: bool,
    /// Whether a cancel button is shown.
    cancel: bool,
}

impl Shown {
    /// Collects what's shown of `widget`; fields get their Ids from `ids`.
    fn collect(&mut self, contents: &impl Contents, widget: &Widget, ids: &FieldIds) {
        match widget.kind() {
            Kind::Column(children) | Kind::Row(children) => {
                children.iter().for_each(|c| self.collect(contents, c, ids));
            }
            Kind::Table { rows, .. } => {
                (rows.iter().flatten()).for_each(|c| self.collect(contents, c, ids));
            }
            Kind::When { state, child } if is_shown(contents, state) => {
                self.collect(contents, child, ids);
            }
            Kind::TextField { id, .. } | Kind::TextArea { id, .. } | Kind::Checkbox { id, .. } => {
                let kind = match widget.kind() {
                    Kind::TextArea { .. } => scope::FieldKind::TextArea,
                    Kind::Checkbox { .. } => {
                        let set = |b| Message(Msg::Toggled(id.clone(), b));
                        scope::FieldKind::Checkbox {
                            checked: contents.fields().checked(id),
                            check: set(true),
                            uncheck: set(false),
                        }
                    }
                    _ => scope::FieldKind::TextField,
                };
                let ids = field_id(ids, id).cloned();
                (self.fields).extend(ids.map(|id| scope::Field { id, kind }));
            }
            Kind::RadioGroup { id, options } => {
                let selected = contents.fields().selected(id);
                let fields = (options.iter().enumerate()).zip(option_ids(ids, id)).map(
                    |((i, option), widget_id)| scope::Field {
                        id: widget_id.clone(),
                        kind: scope::FieldKind::RadioOption {
                            group: id.clone(),
                            selected: selected == Some(&*option.value),
                            select: Message(Msg::Selected(id.clone(), option.value.clone())),
                            digit: option_digit(i),
                            letter: option.key.map(|k| k.char),
                        },
                    },
                );
                self.fields.extend(fields);
            }
            Kind::Button { key, action, .. } => {
                match action {
                    Action::Submit => self.submit = true,
                    Action::Cancel => self.cancel = true,
                    Action::Press(_) | Action::Run(_) => {}
                }
                if let Some(key) = key {
                    self.keys.push((*key, action.clone()));
                }
            }
            _ => {}
        }
    }
}

/// What pressing a button with `action` sends.
fn action_msg(action: Action) -> Msg {
    match action {
        Action::Submit => Msg::Submit,
        Action::Cancel => Msg::Cancel,
        Action::Press(id) => Msg::Press(id),
        Action::Run(id) => Msg::Run(id),
    }
}

/// Collects the ids of the fields in `widget`, in tree order.
fn field_ids(widget: &Widget, out: &mut Vec<Arc<str>>) {
    match widget.kind() {
        Kind::Column(children) | Kind::Row(children) => {
            children.iter().for_each(|c| field_ids(c, out));
        }
        Kind::Table { rows, .. } => rows.iter().flatten().for_each(|c| field_ids(c, out)),
        Kind::When { child, .. } => field_ids(child, out),
        Kind::TextField { id, .. } | Kind::TextArea { id, .. } | Kind::Checkbox { id, .. } => {
            out.push(id.clone());
        }
        // Each option has an Id of its own.
        Kind::RadioGroup { id, options } => out.extend(options.iter().map(|_| id.clone())),
        _ => {}
    }
}

/// The index of the option with `value` in radio group `field`.
fn option_index(widget: &Widget, field: &str, value: &str) -> Option<usize> {
    match widget.kind() {
        Kind::Column(children) | Kind::Row(children) => {
            children.iter().find_map(|c| option_index(c, field, value))
        }
        Kind::Table { rows, .. } => {
            (rows.iter().flatten()).find_map(|c| option_index(c, field, value))
        }
        Kind::When { child, .. } => option_index(child, field, value),
        Kind::RadioGroup { id, options } if **id == *field => {
            options.iter().position(|o| *o.value == *value)
        }
        _ => None,
    }
}

/// The Id of field `field`; of a radio group, its first option's.
fn field_id<'a>(ids: &'a FieldIds, field: &str) -> Option<&'a Id> {
    option_ids(ids, field).next()
}

/// The Ids of the options of radio group `field`, in order (or the Id of
/// another field).
fn option_ids<'a>(ids: &'a FieldIds, field: &str) -> impl Iterator<Item = &'a Id> {
    ids.iter()
        .filter(move |(f, _)| **f == *field)
        .map(|(_, id)| id)
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
        action_button, cancel_button, checkbox, column, label, radio_group, row, state,
        submit_button, text_area, text_field, when,
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
    fn fields_have_ids_in_tree_order() {
        let island = island();
        let fields: Vec<&str> = island.fields.iter().map(|(f, _)| &**f).collect();
        assert_eq!(fields, ["a", "c", "b"]);
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
                                let mut out = format!(
                                    "submitted {:?} {:?}",
                                    fields.text("a"),
                                    fields.text("b")
                                );
                                for c in ["c", "d"].into_iter().filter(|c| fields.checked(c)) {
                                    out += &format!(" {c}");
                                }
                                if let Some(value) = fields.selected("r") {
                                    out += &format!(" r={value}");
                                }
                                out
                            }
                            Outcome::Cancelled => "cancelled".into(),
                            Outcome::Pressed(id, _) => format!("pressed {id}"),
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
        program_with_task(move || (make(), Task::none()), focus)
    }

    /// Like [`program`], but runs the island's task from [`Island::new`] too.
    fn program_with_task(
        make: impl Fn() -> (Island, Task<super::Message>) + 'static,
        focus: Focus,
    ) -> iced::Application<impl iced::Program> {
        let boot = move || {
            let (island, task) = make();
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
            (state, Task::batch([task.map(Msg::Island), check]))
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

    /// A text field and a text area.
    fn notes() -> Island {
        let builder = IslandBuilder::new(column([
            text_field("a", "A"),
            text_area("b", "B"),
            submit_button("OK"),
        ]));
        Island::new(builder).unwrap().0
    }

    #[test]
    fn enter_goes_to_a_text_area_and_starts_new_lines_there() {
        check(
            program(notes, Focus::Field("a")),
            &[
                expect("focused: Some(true)"),
                typewrite("1"),
                press(Key::Enter),
                typewrite("x"),
                press(Key::Enter),
                typewrite("y"),
                // iced_test can't see the text of a text editor.
                expect("outcome: None"),
                Instruction::Interact(Interaction::Mouse(Mouse::Click {
                    button: mouse::Button::Left,
                    target: Some(Target::Text("OK".into())),
                })),
                expect(r#"outcome: Some("submitted \"1\" \"x\\ny\"")"#),
            ],
        );
    }

    #[test]
    fn text_area_takes_changes_from_outside() {
        let mut island = notes();
        let id = island.field_id("b").unwrap();
        let _ = island.update(super::Message(super::Msg::TextChanged(
            "b".into(),
            "old".into(),
        )));
        let messages: Vec<_> = {
            let mut ui = simulator(island.view());
            ui.click(id).unwrap();
            ui.typewrite("!");
            ui.into_messages().collect()
        };
        for message in messages {
            let _ = island.update(message);
        }
        let text = island.freeze().fields().text("b").to_owned();
        assert!(text.contains("old") && text.contains('!'), "{text:?}");
    }

    #[test]
    fn a_frozen_text_area_takes_clicks_and_moves_but_not_edits() {
        // Like a disabled text input, so its text can be selected.
        let builder = IslandBuilder::new(text_area("b", "B"));
        let frozen = Island::new(builder).unwrap().0.freeze();
        let mut ui = simulator(view_frozen::<()>(&frozen));
        ui.point_at((20.0, 10.0));
        let clicked = ui.simulate(iced_test::simulator::click());
        assert!(clicked.contains(&iced::event::Status::Captured));
        assert_eq!(ui.typewrite("x"), iced::event::Status::Ignored);
        assert_eq!(
            ui.tap_key(iced::keyboard::key::Named::ArrowLeft),
            iced::event::Status::Captured
        );
        assert_eq!(ui.into_messages().count(), 0);
    }

    /// Buttons with keys, and no text fields.
    fn question() -> Island {
        let builder = IslandBuilder::new(row([submit_button("Yes [y]"), cancel_button("No [n]")]));
        Island::new(builder).unwrap().0
    }

    /// A text field that must not be empty.
    fn required() -> (Island, Task<super::Message>) {
        let mut builder = IslandBuilder::new(column([
            text_field("a", "A"),
            label(state("error")),
            submit_button("OK"),
        ]));
        builder.validate("error", |i| {
            i.text("a").is_empty().then(|| "A is required".into())
        });
        Island::new(builder).unwrap()
    }

    #[test]
    fn submit_waits_for_validators_to_pass() {
        check(
            program_with_task(required, Focus::Field("a")),
            &[
                expect("focused: Some(true)"),
                press(Key::Enter),
                expect("A is required"),
                expect("outcome: None"),
                typewrite("1"),
                // Validated in the background, so the submit waits for it.
                press(Key::Enter),
                expect(r#"outcome: Some("submitted \"1\" \"\"")"#),
            ],
        );
    }

    #[test]
    fn action_buttons_change_fields_and_keep_the_island_open() {
        let make = || {
            let mut builder = IslandBuilder::new(column([
                text_field("a", "A"),
                row([submit_button("OK"), action_button("double", "Double")]),
            ]));
            builder.set_text("a", "ab");
            builder.action("double", |i, c| c.set_text("a", i.text("a").repeat(2)));
            Island::new(builder).unwrap()
        };
        check(
            program_with_task(make, Focus::Field("a")),
            &[
                expect("focused: Some(true)"),
                Instruction::Interact(Interaction::Mouse(Mouse::Click {
                    button: mouse::Button::Left,
                    target: Some(Target::Text("Double".into())),
                })),
                // Run in the background.
                expect("abab"),
                expect("outcome: None"),
                Instruction::Interact(Interaction::Mouse(Mouse::Click {
                    button: mouse::Button::Left,
                    target: Some(Target::Text("OK".into())),
                })),
                expect(r#"outcome: Some("submitted \"abab\" \"\"")"#),
            ],
        );
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

    /// The events of pressing and releasing Alt and `key`, at the physical
    /// key `code`.
    fn alt(
        key: impl Into<iced::keyboard::Key>,
        code: iced::keyboard::key::Code,
    ) -> Vec<iced::Event> {
        let key = key.into();
        key_events(key.clone(), key, code, iced::keyboard::Modifiers::ALT)
    }

    /// The events of pressing and releasing the physical key `code` with
    /// `modifiers`: `key` without them, `modified_key` with them.
    fn key_events(
        key: iced::keyboard::Key,
        modified_key: iced::keyboard::Key,
        code: iced::keyboard::key::Code,
        modifiers: iced::keyboard::Modifiers,
    ) -> Vec<iced::Event> {
        use iced::keyboard::{self, Location, key::Physical};
        // Unused, so not set right for the numpad.
        let location = Location::Standard;
        vec![
            iced::Event::Keyboard(keyboard::Event::KeyPressed {
                key: key.clone(),
                modified_key: modified_key.clone(),
                physical_key: Physical::Code(code),
                location,
                modifiers,
                text: None,
                repeat: false,
            }),
            iced::Event::Keyboard(keyboard::Event::KeyReleased {
                key,
                modified_key,
                physical_key: Physical::Code(code),
                location,
                modifiers,
            }),
        ]
    }

    #[test]
    fn digit_keys_work_from_the_number_row_and_numpad() {
        use iced::keyboard::key::{Code, Named};
        use iced::keyboard::{Key, Modifiers};

        let builder = IslandBuilder::new(column([
            label("Pick"),
            row((1..=6).map(|i| {
                let label = kettle_ui::widget::ButtonLabel::new(i.to_string());
                let digit = char::from_digit(i, 10).unwrap();
                let label = if i <= 4 {
                    label.key(digit)
                } else {
                    label.alt_key(digit)
                };
                kettle_ui::widget::button(&i.to_string(), label)
            })),
        ]));
        let island = Island::new(builder).unwrap().0;
        let c = |s: &str| Key::Character(s.into());
        let none = Modifiers::empty();
        let messages: Vec<String> = {
            let mut ui = simulator(island.view());
            ui.click("Pick").unwrap();
            // The number row, typing the digit.
            ui.simulate(key_events(c("1"), c("1"), Code::Digit1, none));
            // In Czech, the row types letters, and digits with Shift: both
            // work.
            ui.simulate(key_events(c("ě"), c("ě"), Code::Digit2, none));
            ui.simulate(key_events(c("š"), c("3"), Code::Digit3, Modifiers::SHIFT));
            // In English, Shift+1 types "!": nothing.
            ui.simulate(key_events(c("1"), c("!"), Code::Digit1, Modifiers::SHIFT));
            // The numpad, with Num Lock on, and off (just Left).
            let left = Key::Named(Named::ArrowLeft);
            ui.simulate(key_events(left.clone(), c("4"), Code::Numpad4, none));
            ui.simulate(key_events(left.clone(), left, Code::Numpad4, none));
            // With Alt, the same.
            ui.simulate(alt(c("ř"), Code::Digit5));
            ui.simulate(key_events(
                Key::Named(Named::ArrowRight),
                c("6"),
                Code::Numpad6,
                Modifiers::ALT,
            ));
            ui.into_messages().map(|m| format!("{m:?}")).collect()
        };
        assert_eq!(
            messages,
            ["1", "2", "3", "4", "5", "6"].map(|id| format!(r#"Message(Press("{id}"))"#))
        );
    }

    #[test]
    fn alt_keys_press_buttons_from_a_text_field() {
        use iced::keyboard::{Key, key::Code, key::Named};

        let builder = IslandBuilder::new(column([
            text_field("a", ""),
            row([submit_button("OK [Alt+o]"), cancel_button("Cancel [Alt+c]")]),
        ]));
        let island = Island::new(builder).unwrap().0;
        let a = island.field_id("a").unwrap();
        let messages: Vec<String> = {
            let mut ui = simulator(island.view());
            // Without focus, nothing.
            ui.simulate(alt(Key::Character("o".into()), Code::KeyO));
            ui.click(a).unwrap();
            // Plain letters are typed.
            ui.typewrite("c");
            // No button has it.
            ui.simulate(alt(Key::Character("x".into()), Code::KeyX));
            ui.simulate(alt(Key::Character("o".into()), Code::KeyO));
            // In another layout, the key at the same place.
            ui.simulate(alt(Key::Character("с".into()), Code::KeyC));
            ui.tap_key(Key::Named(Named::Escape));
            ui.into_messages().map(|m| format!("{m:?}")).collect()
        };
        assert_eq!(
            messages,
            [
                r#"Message(TextChanged("a", "c"))"#,
                "Message(Submit)",
                "Message(Cancel)",
                "Message(Cancel)",
            ]
        );
    }

    #[test]
    fn alt_enter_submits_from_any_field() {
        use iced::keyboard::{key::Code, key::Named};

        let island = notes();
        let fields = [island.field_id("a").unwrap(), island.field_id("b").unwrap()];
        let messages: Vec<String> = {
            let mut ui = simulator(island.view());
            // Without focus, nothing.
            ui.simulate(alt(Named::Enter, Code::Enter));
            // From the first of two fields, and from a text area.
            for id in fields {
                ui.click(id).unwrap();
                ui.simulate(alt(Named::Enter, Code::Enter));
            }
            ui.into_messages().map(|m| format!("{m:?}")).collect()
        };
        assert_eq!(messages, ["Message(Submit)", "Message(Submit)"]);
    }

    #[test]
    fn a_key_and_the_key_with_alt_are_separate() {
        use iced::keyboard::{Key, key::Code};

        let builder = IslandBuilder::new(column([
            label("Stop?"),
            row([
                kettle_ui::widget::button("pause", "Pause [p]"),
                kettle_ui::widget::button("quit", "Quit [Alt+p]"),
            ]),
        ]));
        let island = Island::new(builder).unwrap().0;
        let messages: Vec<String> = {
            let mut ui = simulator(island.view());
            ui.click("Stop?").unwrap();
            ui.typewrite("p");
            ui.simulate(alt(Key::Character("p".into()), Code::KeyP));
            ui.into_messages().map(|m| format!("{m:?}")).collect()
        };
        assert_eq!(
            messages,
            [r#"Message(Press("pause"))"#, r#"Message(Press("quit"))"#]
        );
    }

    /// A text field, then two checkboxes, the first one checked.
    fn toppings() -> Island {
        let mut builder = IslandBuilder::new(column([
            text_field("a", "A"),
            checkbox("c", "C"),
            checkbox("d", "D"),
            row([submit_button("OK"), cancel_button("Cancel")]),
        ]));
        builder.set_checked("c", true);
        Island::new(builder).unwrap().0
    }

    #[test]
    fn enter_in_a_checkbox_skips_the_checkboxes_after_it() {
        fn form() -> Island {
            let builder = IslandBuilder::new(column([
                checkbox("c", "C"),
                checkbox("d", "D"),
                text_field("a", "A"),
                checkbox("e", "E"),
                checkbox("f", "F"),
                submit_button("OK"),
            ]));
            Island::new(builder).unwrap().0
        }
        check(
            program(form, Focus::Field("c")),
            &[
                expect("focused: Some(true)"),
                press(Key::Enter),
                typewrite("1"),
                // On to e, then Enter there submits.
                press(Key::Enter),
                expect("outcome: None"),
                press(Key::Enter),
                expect(r#"outcome: Some("submitted \"1\" \"\"")"#),
            ],
        );
    }

    #[test]
    fn enter_goes_to_the_first_checkbox_then_submits() {
        check(
            program(toppings, Focus::Field("a")),
            &[
                expect("focused: Some(true)"),
                typewrite("1"),
                press(Key::Enter),
                press(Key::Enter),
                expect(r#"outcome: Some("submitted \"1\" \"\" c")"#),
            ],
        );
    }

    #[test]
    fn y_and_n_set_checkboxes_and_move_on_but_never_submit() {
        check(
            program(toppings, Focus::Field("c")),
            &[
                expect("focused: Some(true)"),
                typewrite("n"),
                typewrite("y"),
                // Now the island as a whole has focus.
                typewrite("y"),
                expect("outcome: None"),
                press(Key::Enter),
                expect(r#"outcome: Some("submitted \"\" \"\" d")"#),
            ],
        );
    }

    #[test]
    fn island_focus_goes_to_the_first_checkbox() {
        fn checkboxes() -> Island {
            let builder = IslandBuilder::new(column([checkbox("c", "C"), submit_button("OK")]));
            Island::new(builder).unwrap().0
        }
        check(
            program(checkboxes, Focus::Island),
            &[
                expect("focused: Some(true)"),
                typewrite("y"),
                press(Key::Enter),
                expect(r#"outcome: Some("submitted \"\" \"\" c")"#),
            ],
        );
    }

    #[test]
    fn space_toggles_and_arrows_move_between_fields() {
        use iced::keyboard::{Key, key::Named};

        let island = toppings();
        let d = island.field_id("d").unwrap();
        let mut ui = simulator(island.view());
        // Clicking focuses it, and toggles it.
        ui.click(d).unwrap();
        ui.tap_key(Key::Named(Named::ArrowUp));
        ui.tap_key(Key::Named(Named::Space));
        ui.tap_key(Key::Named(Named::ArrowDown));
        ui.tap_key(Key::Named(Named::ArrowDown));
        // The island as a whole, where Enter submits.
        ui.tap_key(Key::Named(Named::Space));
        ui.tap_key(Key::Named(Named::Enter));
        ui.tap_key(Key::Named(Named::ArrowUp));
        ui.tap_key(Key::Named(Named::Space));
        // Up from a checkbox goes to a text field too.
        ui.tap_key(Key::Named(Named::ArrowUp));
        ui.tap_key(Key::Named(Named::ArrowUp));
        ui.typewrite("x");
        let messages: Vec<String> = ui.into_messages().map(|m| format!("{m:?}")).collect();
        assert_eq!(
            messages,
            [
                r#"Message(Toggled("d", true))"#,
                r#"Message(Toggled("c", false))"#,
                "Message(Submit)",
                r#"Message(Toggled("d", true))"#,
                r#"Message(TextChanged("a", "x"))"#,
            ]
        );
    }

    /// Pressing and releasing the named key with Ctrl (Cmd on macOS).
    fn ctrl(named: iced::keyboard::key::Named) -> [iced::Event; 2] {
        use iced::keyboard::key::{NativeCode, Physical};
        use iced::keyboard::{Event, Key, Location, Modifiers};

        let key = Key::Named(named);
        let physical_key = Physical::Unidentified(NativeCode::Unidentified);
        [
            iced::Event::Keyboard(Event::KeyPressed {
                key: key.clone(),
                modified_key: key.clone(),
                physical_key,
                location: Location::Standard,
                modifiers: Modifiers::COMMAND,
                repeat: false,
                text: None,
            }),
            iced::Event::Keyboard(Event::KeyReleased {
                modified_key: key.clone(),
                key,
                physical_key,
                location: Location::Standard,
                modifiers: Modifiers::COMMAND,
            }),
        ]
    }

    #[test]
    fn ctrl_arrows_in_a_checkbox_skip_its_checkboxes() {
        use iced::keyboard::{Key, key::Named};

        let builder = IslandBuilder::new(column([
            checkbox("c", "C"),
            checkbox("d", "D"),
            text_field("a", "A"),
            checkbox("e", "E"),
            checkbox("f", "F"),
            submit_button("OK"),
        ]));
        let island = Island::new(builder).unwrap().0;
        let d = island.field_id("d").unwrap();
        let mut ui = simulator(island.view());
        // Clicking focuses it, and toggles it.
        ui.click(d).unwrap();
        // Nothing before c and d, so focus stays on d (the view isn't
        // rebuilt, so Space checks it again).
        ui.simulate(ctrl(Named::ArrowUp));
        ui.tap_key(Key::Named(Named::Space));
        ui.simulate(ctrl(Named::ArrowDown));
        ui.typewrite("1");
        ui.tap_key(Key::Named(Named::ArrowDown));
        ui.tap_key(Key::Named(Named::ArrowDown));
        // From f, back past e to a.
        ui.simulate(ctrl(Named::ArrowUp));
        ui.typewrite("2");
        ui.tap_key(Key::Named(Named::ArrowDown));
        // From e, past f to the island as a whole, where Enter submits.
        ui.simulate(ctrl(Named::ArrowDown));
        ui.tap_key(Key::Named(Named::Space));
        ui.tap_key(Key::Named(Named::Enter));
        let messages: Vec<String> = ui.into_messages().map(|m| format!("{m:?}")).collect();
        assert_eq!(
            messages,
            [
                r#"Message(Toggled("d", true))"#,
                r#"Message(Toggled("d", true))"#,
                r#"Message(TextChanged("a", "1"))"#,
                r#"Message(TextChanged("a", "12"))"#,
                "Message(Submit)",
            ]
        );
    }

    #[test]
    fn ctrl_arrows_return_to_the_last_focused_checkbox() {
        use iced::keyboard::{Key, key::Named};

        let builder = IslandBuilder::new(column([
            checkbox("c", "C"),
            checkbox("d", "D"),
            checkbox("e", "E"),
            text_field("a", "A"),
            checkbox("f", "F"),
            checkbox("g", "G"),
            submit_button("OK"),
        ]));
        let island = Island::new(builder).unwrap().0;
        let d = island.field_id("d").unwrap();
        let a = island.field_id("a").unwrap();
        let mut ui = simulator(island.view());
        // Clicking d (which toggles it), then a.
        ui.click(d).unwrap();
        ui.click(a).unwrap();
        ui.typewrite("1");
        // Back to d, not e (the view isn't rebuilt, so Space checks it
        // again).
        ui.simulate(ctrl(Named::ArrowUp));
        ui.tap_key(Key::Named(Named::Space));
        // Down to e, then a, and back to e.
        ui.tap_key(Key::Named(Named::ArrowDown));
        ui.tap_key(Key::Named(Named::ArrowDown));
        ui.simulate(ctrl(Named::ArrowUp));
        ui.tap_key(Key::Named(Named::Space));
        // Past the run to a, then to f, the first of a run without one.
        ui.simulate(ctrl(Named::ArrowDown));
        ui.typewrite("2");
        ui.simulate(ctrl(Named::ArrowDown));
        ui.tap_key(Key::Named(Named::ArrowDown));
        // From g to the island as a whole, and back to g.
        ui.tap_key(Key::Named(Named::ArrowDown));
        ui.simulate(ctrl(Named::ArrowUp));
        ui.tap_key(Key::Named(Named::Space));
        // Back up to a, then down to g again.
        ui.simulate(ctrl(Named::ArrowUp));
        ui.typewrite("3");
        ui.simulate(ctrl(Named::ArrowDown));
        ui.tap_key(Key::Named(Named::Space));
        let messages: Vec<String> = ui.into_messages().map(|m| format!("{m:?}")).collect();
        assert_eq!(
            messages,
            [
                r#"Message(Toggled("d", true))"#,
                r#"Message(TextChanged("a", "1"))"#,
                r#"Message(Toggled("d", true))"#,
                r#"Message(Toggled("e", true))"#,
                r#"Message(TextChanged("a", "12"))"#,
                r#"Message(Toggled("g", true))"#,
                r#"Message(TextChanged("a", "123"))"#,
                r#"Message(Toggled("g", true))"#,
            ]
        );
    }

    #[test]
    fn radio_options_are_selected_by_space_and_their_keys() {
        use iced::keyboard::key::{Code, Named};
        use iced::keyboard::{Key, Modifiers};

        let mut builder = IslandBuilder::new(column([
            radio_group(
                "size",
                [("s", "Small [s]"), ("m", "Medium"), ("l", "Large")],
            ),
            checkbox("c", "C"),
            submit_button("OK"),
        ]));
        builder.set_selected("size", "m");
        let island = Island::new(builder).unwrap().0;
        // The selected option's.
        let m = island.field_id("size").unwrap();
        let c = |s: &str| Key::Character(s.into());
        let mut ui = simulator(island.view());
        // Clicking focuses it, and selects it (again).
        ui.click(m).unwrap();
        // It's selected (the view isn't rebuilt, so it stays so).
        ui.tap_key(Key::Named(Named::Space));
        ui.tap_key(Key::Named(Named::ArrowUp));
        ui.tap_key(Key::Named(Named::Space));
        // Its number, its letter, and the number of one with a letter.
        ui.typewrite("3");
        ui.typewrite("s");
        ui.typewrite("1");
        // The number row in Czech.
        ui.simulate(key_events(c("ě"), c("ě"), Code::Digit2, Modifiers::empty()));
        // Not a key of the group.
        ui.typewrite("y");
        ui.typewrite("4");
        // Enter skips the group, without selecting.
        ui.tap_key(Key::Named(Named::Enter));
        ui.tap_key(Key::Named(Named::Space));
        // Back to the group, where the key focused m.
        ui.tap_key(Key::Named(Named::ArrowUp));
        ui.tap_key(Key::Named(Named::Space));
        let messages: Vec<String> = ui.into_messages().map(|m| format!("{m:?}")).collect();
        assert_eq!(
            messages,
            [
                r#"Message(Selected("size", "m"))"#,
                r#"Message(Selected("size", "s"))"#,
                r#"Message(Selected("size", "l"))"#,
                r#"Message(Selected("size", "s"))"#,
                r#"Message(Selected("size", "s"))"#,
                r#"Message(Toggled("c", true))"#,
                r#"Message(Selected("size", "l"))"#,
            ]
        );
    }

    #[test]
    fn ctrl_arrows_land_on_the_selected_option() {
        use iced::keyboard::{Key, key::Named};

        let mut builder = IslandBuilder::new(column([
            checkbox("c", "C"),
            radio_group("r", [("a", "A"), ("b", "B"), ("c", "C")]),
            radio_group("s", [("a", "A"), ("b", "B")]),
            text_field("t", "T"),
            submit_button("OK"),
        ]));
        builder.set_selected("r", "b");
        let island = Island::new(builder).unwrap().0;
        let checkbox_id = island.field_id("c").unwrap();
        let mut ui = simulator(island.view());
        ui.click(checkbox_id).unwrap();
        // From the checkbox to b, the selected option; Down from there to c.
        ui.simulate(ctrl(Named::ArrowDown));
        ui.tap_key(Key::Named(Named::ArrowDown));
        ui.tap_key(Key::Named(Named::Space));
        // Past r to s, a group of its own; nothing selected there, so its
        // first option.
        ui.simulate(ctrl(Named::ArrowDown));
        ui.tap_key(Key::Named(Named::Space));
        // Down to b, then past s to t, and back to b, the last one focused.
        ui.tap_key(Key::Named(Named::ArrowDown));
        ui.simulate(ctrl(Named::ArrowDown));
        ui.typewrite("x");
        ui.simulate(ctrl(Named::ArrowUp));
        ui.tap_key(Key::Named(Named::Space));
        // Past s, and back to r's selected option, b.
        ui.simulate(ctrl(Named::ArrowUp));
        ui.tap_key(Key::Named(Named::ArrowUp));
        ui.tap_key(Key::Named(Named::Space));
        let messages: Vec<String> = ui.into_messages().map(|m| format!("{m:?}")).collect();
        assert_eq!(
            messages,
            [
                r#"Message(Toggled("c", true))"#,
                r#"Message(Selected("r", "c"))"#,
                r#"Message(Selected("s", "a"))"#,
                r#"Message(TextChanged("t", "x"))"#,
                r#"Message(Selected("s", "b"))"#,
                r#"Message(Selected("r", "a"))"#,
            ]
        );
    }

    #[test]
    fn island_focus_goes_to_a_radio_group() {
        fn form() -> Island {
            let mut builder = IslandBuilder::new(column([
                radio_group("r", [("a", "A"), ("b", "B"), ("c", "C")]),
                submit_button("OK"),
            ]));
            builder.set_selected("r", "b");
            Island::new(builder).unwrap().0
        }
        check(
            program(form, Focus::Island),
            &[
                expect("focused: Some(true)"),
                // It's the group that has focus.
                typewrite("3"),
                press(Key::Enter),
                expect(r#"outcome: Some("submitted \"\" \"\" r=c")"#),
            ],
        );
    }

    #[test]
    fn arrows_move_between_text_fields_and_ctrl_arrows_out_of_text_areas() {
        use iced::keyboard::{Key, key::Named};

        fn form() -> Island {
            let builder = IslandBuilder::new(column([
                text_field("a", "A"),
                text_field("b", "B"),
                text_area("c", "C"),
                submit_button("OK"),
            ]));
            Island::new(builder).unwrap().0
        }
        let island = form();
        let a = island.field_id("a").unwrap();
        let mut ui = simulator(island.view());
        ui.click(a).unwrap();
        ui.tap_key(Key::Named(Named::ArrowDown));
        ui.typewrite("1");
        ui.tap_key(Key::Named(Named::ArrowUp));
        ui.typewrite("2");
        ui.tap_key(Key::Named(Named::ArrowDown));
        ui.tap_key(Key::Named(Named::ArrowDown));
        ui.typewrite("x");
        // In a text area, Up and Down move between lines.
        ui.tap_key(Key::Named(Named::ArrowUp));
        ui.tap_key(Key::Named(Named::ArrowDown));
        ui.typewrite("y");
        // Ctrl+Up and Ctrl+Down move between fields, there and elsewhere.
        ui.simulate(ctrl(Named::ArrowUp));
        ui.typewrite("3");
        ui.simulate(ctrl(Named::ArrowDown));
        ui.typewrite("z");
        ui.simulate(ctrl(Named::ArrowDown));
        ui.tap_key(Key::Named(Named::Enter));
        let messages: Vec<String> = ui.into_messages().map(|m| format!("{m:?}")).collect();
        assert_eq!(
            messages,
            [
                r#"Message(TextChanged("b", "1"))"#,
                r#"Message(TextChanged("a", "2"))"#,
                r#"Message(TextChanged("c", "x"))"#,
                r#"Message(TextChanged("c", "xy"))"#,
                r#"Message(TextChanged("b", "13"))"#,
                r#"Message(TextChanged("c", "xyz"))"#,
                "Message(Submit)",
            ]
        );
    }

    #[test]
    fn numpad_digits_with_num_lock_type_instead_of_moving() {
        use iced::keyboard::key::{Code, Named, Physical};
        use iced::keyboard::{Key, Location, Modifiers};

        let builder = IslandBuilder::new(column([
            text_field("a", "A"),
            text_field("b", "B"),
            submit_button("OK"),
        ]));
        let island = Island::new(builder).unwrap().0;
        let a = island.field_id("a").unwrap();
        let mut ui = simulator(island.view());
        ui.click(a).unwrap();
        // Numpad 2 with Num Lock on: without modifiers, it's still Down.
        let key = Key::Named(Named::ArrowDown);
        let modified_key = Key::Character("2".into());
        let physical_key = Physical::Code(Code::Numpad2);
        ui.simulate([
            iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                key: key.clone(),
                modified_key: modified_key.clone(),
                physical_key,
                location: Location::Numpad,
                modifiers: Modifiers::empty(),
                text: Some("2".into()),
                repeat: false,
            }),
            iced::Event::Keyboard(iced::keyboard::Event::KeyReleased {
                key,
                modified_key,
                physical_key,
                location: Location::Numpad,
                modifiers: Modifiers::empty(),
            }),
        ]);
        let messages: Vec<String> = ui.into_messages().map(|m| format!("{m:?}")).collect();
        assert_eq!(messages, [r#"Message(TextChanged("a", "2"))"#]);
    }

    #[test]
    fn down_from_the_last_text_field_focuses_the_island() {
        use iced::keyboard::{Key, key::Named};

        let builder = IslandBuilder::new(column([text_field("a", "A"), submit_button("OK")]));
        let island = Island::new(builder).unwrap().0;
        let a = island.field_id("a").unwrap();
        let mut ui = simulator(island.view());
        ui.click(a).unwrap();
        ui.tap_key(Key::Named(Named::ArrowUp));
        ui.typewrite("1");
        ui.tap_key(Key::Named(Named::ArrowDown));
        ui.tap_key(Key::Named(Named::ArrowDown));
        // The island as a whole: typing does nothing, Enter submits.
        ui.typewrite("2");
        ui.tap_key(Key::Named(Named::Enter));
        ui.tap_key(Key::Named(Named::ArrowUp));
        ui.typewrite("3");
        let messages: Vec<String> = ui.into_messages().map(|m| format!("{m:?}")).collect();
        assert_eq!(
            messages,
            [
                r#"Message(TextChanged("a", "1"))"#,
                "Message(Submit)",
                r#"Message(TextChanged("a", "13"))"#,
            ]
        );
    }

    /// A dangerous question, with a button that's not a submit button.
    fn dangerous() -> Island {
        let builder = IslandBuilder::new(row([
            kettle_ui::widget::button("del", "Delete [d]"),
            cancel_button("Keep [k]"),
        ]));
        Island::new(builder).unwrap().0
    }

    #[test]
    fn enter_does_not_press_a_plain_button() {
        check(
            program(dangerous, Focus::Island),
            &[
                expect("focused: Some(true)"),
                press(Key::Enter),
                expect("outcome: None"),
                typewrite("d"),
                expect(r#"outcome: Some("pressed del")"#),
            ],
        );
    }
}
