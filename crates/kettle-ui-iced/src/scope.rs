//! [`Scope`]: wraps a live island, and handles its keys.

use std::ops::Range;
use std::sync::Arc;

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::{self, Renderer as _};
use iced::advanced::widget::operation::{Focusable, focusable};
use iced::advanced::widget::{Id, Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, mouse, overlay};
use iced::keyboard::{self, key};
use iced::{
    Border, Color, Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector, touch,
};

use kettle_ui::widget::ButtonKey;

use crate::Message;

/// Wraps the UI of a live island, and handles its keys before its widgets
/// see them. Checkboxes next to each other form a run, and so do the options
/// of a radio group (each option is a field).
/// - Enter in a text field focuses the next field; in the last, it submits.
///   Ctrl+Enter does the same, also in a text area (where Enter starts a new
///   line). In a run, Enter skips the rest of it: it focuses the field after
///   the run, or submits if there's none. It never selects a radio option.
/// - In a checkbox: Space toggles it; `y` checks it and `n` unchecks it, and
///   both move on to the next field, or after the last one to the island as
///   a whole.
/// - In a run, `j` and `k` focus the next and previous field of the run, but
///   never leave it (a radio option's letter `j` or `k` selects it instead).
/// - In a radio option: Space selects it; an option's number (`1` to `9`)
///   or letter selects it and moves on past the group, to the next field or
///   else the island as a whole.
/// - In a text field, checkbox or radio option (not a text area), Up and
///   Down focus the previous and next field; Down from the last one focuses
///   the island as a whole. Ctrl+Up and Ctrl+Down do the same, also in a
///   text area (where Up and Down move between lines); in a run, they skip
///   the rest of it (Ctrl+Up does nothing if there's no field before it).
///   When they land on a run, they focus its selected radio option, or else
///   the field in it that last had focus, if any.
/// - Escape, while the island has focus, cancels.
/// - While the island as a whole has focus, Enter submits, a button's key
///   (without Alt) presses it, and Up (if there are fields) focuses the
///   last field.
/// - A button's key with Alt presses it while the island or one of its
///   fields has focus, and Alt+Enter submits.
///
/// An island without fields can have focus as a whole: like a text field, it
/// gets it when clicked or through iced's focus operations (with its own
/// [`Id`]), and loses it on a click elsewhere. An island with fields gets it
/// only from its last field (by `y`/`n` or Down).
pub(crate) struct Scope<'a> {
    pub content: Element<'a, Message>,
    /// The island's Id, for focus as a whole.
    pub id: Id,
    /// Whether the island has no fields, so a click focuses it as a whole.
    pub click_focus: bool,
    /// The shown fields, in tree order.
    pub fields: Vec<Field>,
    /// The keys of shown buttons, and what pressing them sends.
    pub keys: Vec<(ButtonKey, Message)>,
    /// What a shown submit button sends, if there is one.
    pub submit: Option<Message>,
    /// What a shown cancel button sends, if there is one.
    pub cancel: Option<Message>,
}

/// A shown field: a text field or area, a checkbox, or an option of a radio
/// group.
pub(crate) struct Field {
    pub id: Id,
    pub kind: FieldKind,
}

/// What kind of field a [`Field`] is.
pub(crate) enum FieldKind {
    TextField,
    /// Enter starts a new line in it.
    TextArea,
    Checkbox {
        checked: bool,
        /// What checking it sends.
        check: Message,
        /// What unchecking it sends.
        uncheck: Message,
    },
    RadioOption {
        /// The radio group's id.
        group: Arc<str>,
        selected: bool,
        /// What selecting it sends.
        select: Message,
        /// The digit that selects it, if any.
        digit: Option<char>,
        /// The letter that selects it, if any.
        letter: Option<char>,
    },
}

impl FieldKind {
    /// Whether this field and `other` are in the same run, if next to each
    /// other: both checkboxes, or options of the same radio group.
    fn same_run(&self, other: &FieldKind) -> bool {
        match (self, other) {
            (FieldKind::Checkbox { .. }, FieldKind::Checkbox { .. }) => true,
            (FieldKind::RadioOption { group: a, .. }, FieldKind::RadioOption { group: b, .. }) => {
                a == b
            }
            _ => false,
        }
    }

    /// Whether it's a checkbox or a radio option, whose runs are
    /// remembered.
    fn is_choice(&self) -> bool {
        matches!(
            self,
            FieldKind::Checkbox { .. } | FieldKind::RadioOption { .. }
        )
    }
}

/// The run of fields that the field at `field` is in (just that field if
/// it's a text field).
pub(crate) fn run(fields: &[Field], field: usize) -> Range<usize> {
    let kind = &fields[field].kind;
    let start = fields[..field]
        .iter()
        .rposition(|f| !f.kind.same_run(kind))
        .map_or(0, |i| i + 1);
    let end = fields[field + 1..]
        .iter()
        .position(|f| !f.kind.same_run(kind))
        .map_or(fields.len(), |i| field + 1 + i);
    start..end
}

/// The selected radio option in `fields`, if any.
pub(crate) fn selected_option(fields: &[Field]) -> Option<&Field> {
    fields
        .iter()
        .find(|f| matches!(f.kind, FieldKind::RadioOption { selected: true, .. }))
}

/// Whether the island has focus as a whole, and which checkboxes and radio
/// options had focus last.
#[derive(Default)]
struct State {
    focused: bool,
    /// The field that last had focus in each run, oldest first. Runs can
    /// change as fields are shown and hidden, so a run may hold more than
    /// one; the newest counts.
    last_in_runs: Vec<Id>,
}

impl Focusable for State {
    fn is_focused(&self) -> bool {
        self.focused
    }

    fn focus(&mut self) {
        self.focused = true;
    }

    fn unfocus(&mut self) {
        self.focused = false;
    }
}

/// What a key press does: sends a message, moves focus, or both.
#[derive(Default)]
struct KeyAction {
    send: Option<Message>,
    focus: Option<Focus>,
}

/// Where focus goes.
enum Focus {
    Field(Id),
    /// The island as a whole.
    Whole,
}

impl KeyAction {
    fn send(message: Message) -> Self {
        KeyAction {
            send: Some(message),
            focus: None,
        }
    }

    fn focus(focus: Focus) -> Self {
        KeyAction {
            send: None,
            focus: Some(focus),
        }
    }
}

impl Scope<'_> {
    /// Which field has focus, if any; if it's in a run, remembers it as the
    /// last one with focus there.
    fn focused_field(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
    ) -> Option<usize> {
        if self.fields.is_empty() {
            return None;
        }
        let mut find = FindField {
            fields: &self.fields,
            focused: None,
        };
        (self.content.as_widget_mut()).operate(&mut tree.children[0], layout, renderer, &mut find);
        let field = find.focused?;
        if self.fields[field].kind.is_choice() {
            let state = tree.state.downcast_mut::<State>();
            let ids = &self.fields[run(&self.fields, field)];
            state
                .last_in_runs
                .retain(|id| !ids.iter().any(|f| f.id == *id));
            state.last_in_runs.push(self.fields[field].id.clone());
        }
        Some(field)
    }

    /// Where Ctrl+Up or Ctrl+Down landing on the field at `field` focuses:
    /// in a run, its selected radio option, or else the field that last had
    /// focus there.
    fn land(&self, state: &State, field: usize) -> Focus {
        let fields = &self.fields[run(&self.fields, field)];
        let last = || {
            let mut last = state.last_in_runs.iter().rev();
            last.find(|id| fields.iter().any(|f| f.id == **id))
        };
        let id = (selected_option(fields).map(|f| &f.id))
            .or_else(last)
            .unwrap_or(&self.fields[field].id);
        Focus::Field(id.clone())
    }

    /// What pressing `key` (at `physical_key`, a button key's `digit` if
    /// any) with `modifiers` does, if anything, while `field` has focus.
    fn key_action(
        &mut self,
        tree: &Tree,
        field: Option<usize>,
        key: &keyboard::Key,
        physical_key: key::Physical,
        digit: Option<char>,
        modifiers: keyboard::Modifiers,
    ) -> Option<KeyAction> {
        // Shift may be how the layout types the digit.
        let modifiers = match digit {
            Some(_) => modifiers.difference(keyboard::Modifiers::SHIFT),
            None => modifiers,
        };
        let plain = modifiers.is_empty();
        let alt = modifiers == keyboard::Modifiers::ALT;
        if !plain && !alt && modifiers != keyboard::Modifiers::COMMAND {
            return None;
        }
        let state = tree.state.downcast_ref::<State>();
        let whole = state.focused;
        let kind = field.map(|i| &self.fields[i].kind);
        let checkbox = match kind {
            Some(FieldKind::Checkbox {
                checked,
                check,
                uncheck,
            }) => Some((*checked, check, uncheck)),
            _ => None,
        };
        let in_radio_group = matches!(kind, Some(FieldKind::RadioOption { .. }));
        // There, Up and Down move between lines, and Ctrl+Up and Ctrl+Down
        // between fields.
        let in_text_area = matches!(kind, Some(FieldKind::TextArea));
        let arrows_move_focus = !(plain && in_text_area);
        // The field after the focused one, or else the island as a whole.
        let next = |field: usize| match self.fields.get(field + 1) {
            Some(next) => Focus::Field(next.id.clone()),
            None => Focus::Whole,
        };
        // `j` and `k` go to the next and previous field of a run (not past
        // it, so the other key always leads back), unless they are keys of
        // a radio option there.
        if plain
            && digit.is_none()
            && let Some(field) = field
            && self.fields[field].kind.is_choice()
            && let Some(c @ ('j' | 'k')) = single_char(key)
            && self.option_with_key(field, c).is_none()
        {
            let run = run(&self.fields, field);
            let to = match c {
                'j' => Some(field + 1),
                _ => field.checked_sub(1),
            };
            return Some(match to.filter(|to| run.contains(to)) {
                Some(to) => KeyAction::focus(Focus::Field(self.fields[to].id.clone())),
                None => KeyAction::default(),
            });
        }
        match key.as_ref() {
            // Like other Alt shortcuts, it works with any keyboard layout.
            keyboard::Key::Character(_) if alt && (whole || field.is_some()) => {
                let char = match digit {
                    Some(digit) => digit,
                    None => key.to_latin(physical_key)?.to_ascii_lowercase(),
                };
                self.press(ButtonKey { char, alt: true })
            }
            keyboard::Key::Named(key::Named::Enter) if alt && (whole || field.is_some()) => {
                self.submit.clone().map(KeyAction::send)
            }
            _ if alt => None,
            keyboard::Key::Named(key::Named::Escape) if plain && (whole || field.is_some()) => {
                self.cancel.clone().map(KeyAction::send)
            }
            keyboard::Key::Named(key::Named::Enter) if whole => {
                self.submit.clone().map(KeyAction::send)
            }
            keyboard::Key::Named(key::Named::Enter) => {
                let field = field?;
                if plain && in_text_area {
                    return None;
                }
                // Skip the rest of the run.
                match self.fields.get(run(&self.fields, field).end) {
                    Some(next) => Some(KeyAction::focus(Focus::Field(next.id.clone()))),
                    None => self.submit.clone().map(KeyAction::send),
                }
            }
            keyboard::Key::Named(key::Named::Space) if plain => match kind? {
                FieldKind::Checkbox {
                    checked,
                    check,
                    uncheck,
                } => Some(KeyAction::send(
                    if *checked { uncheck } else { check }.clone(),
                )),
                FieldKind::RadioOption {
                    selected, select, ..
                } => Some(KeyAction {
                    send: (!selected).then(|| select.clone()),
                    focus: None,
                }),
                _ => None,
            },
            // In a run, Ctrl+Up skips the rest of it.
            keyboard::Key::Named(key::Named::ArrowUp)
                if !plain && (checkbox.is_some() || in_radio_group) =>
            {
                let previous = run(&self.fields, field?).start.checked_sub(1)?;
                Some(KeyAction::focus(self.land(state, previous)))
            }
            keyboard::Key::Named(key::Named::ArrowUp) if arrows_move_focus => {
                let previous = match field {
                    _ if whole && !self.click_focus => self.fields.len().checked_sub(1)?,
                    Some(field) => field.checked_sub(1)?,
                    None => return None,
                };
                Some(KeyAction::focus(if plain {
                    Focus::Field(self.fields[previous].id.clone())
                } else {
                    self.land(state, previous)
                }))
            }
            // In a run, Ctrl+Down skips the rest of it.
            keyboard::Key::Named(key::Named::ArrowDown)
                if !plain && (checkbox.is_some() || in_radio_group) =>
            {
                let next = run(&self.fields, field?).end;
                Some(KeyAction::focus(if next < self.fields.len() {
                    self.land(state, next)
                } else {
                    Focus::Whole
                }))
            }
            keyboard::Key::Named(key::Named::ArrowDown) if arrows_move_focus => {
                let field = field?;
                Some(KeyAction::focus(
                    if !plain && field + 1 < self.fields.len() {
                        self.land(state, field + 1)
                    } else {
                        next(field)
                    },
                ))
            }
            keyboard::Key::Character(_) if plain && in_radio_group => {
                let c = match digit {
                    Some(digit) => digit,
                    None => single_char(key)?,
                };
                let option = self.option_with_key(field?, c)?;
                let FieldKind::RadioOption {
                    selected, select, ..
                } = &option.kind
                else {
                    unreachable!("a radio group has only radio options");
                };
                // Past the group, like `y` and `n` in a checkbox.
                let last = run(&self.fields, field?).end - 1;
                Some(KeyAction {
                    send: (!selected).then(|| select.clone()),
                    focus: Some(next(last)),
                })
            }
            keyboard::Key::Character(_) if plain && (whole || checkbox.is_some()) => {
                let c = single_char(key)?;
                if let Some((checked, check, uncheck)) = checkbox {
                    let (message, value) = match c {
                        'y' => (check, true),
                        'n' => (uncheck, false),
                        _ => return None,
                    };
                    return Some(KeyAction {
                        send: (checked != value).then(|| message.clone()),
                        focus: Some(next(field?)),
                    });
                }
                self.press(ButtonKey {
                    char: digit.unwrap_or(c),
                    alt: false,
                })
            }
            _ => None,
        }
    }

    /// The radio option whose number or letter is `c`, in the run of the
    /// field at `field`.
    fn option_with_key(&self, field: usize, c: char) -> Option<&Field> {
        self.fields[run(&self.fields, field)]
            .iter()
            .find(|f| match &f.kind {
                FieldKind::RadioOption { digit, letter, .. } => {
                    *digit == Some(c) || *letter == Some(c)
                }
                _ => false,
            })
    }

    /// Presses the button with `key`, if there's one.
    fn press(&self, key: ButtonKey) -> Option<KeyAction> {
        let (_, message) = self.keys.iter().find(|(k, _)| *k == key)?;
        Some(KeyAction::send(message.clone()))
    }
}

/// The character `key` types, lowercased, if it's a single one.
fn single_char(key: &keyboard::Key) -> Option<char> {
    let keyboard::Key::Character(s) = key.as_ref() else {
        return None;
    };
    let mut chars = s.chars().map(|c| c.to_ascii_lowercase());
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

/// The digit that a key press stands for as a button key: a key of the
/// number row, whatever the layout types there (but with Shift only if that
/// types the digit), or of the numpad, if it types the digit (with Num
/// Lock on).
fn digit(
    modified_key: &keyboard::Key,
    physical_key: key::Physical,
    modifiers: keyboard::Modifiers,
) -> Option<char> {
    use key::Code;
    const ROW: [Code; 10] = [
        Code::Digit0,
        Code::Digit1,
        Code::Digit2,
        Code::Digit3,
        Code::Digit4,
        Code::Digit5,
        Code::Digit6,
        Code::Digit7,
        Code::Digit8,
        Code::Digit9,
    ];
    const NUMPAD: [Code; 10] = [
        Code::Numpad0,
        Code::Numpad1,
        Code::Numpad2,
        Code::Numpad3,
        Code::Numpad4,
        Code::Numpad5,
        Code::Numpad6,
        Code::Numpad7,
        Code::Numpad8,
        Code::Numpad9,
    ];
    let key::Physical::Code(code) = physical_key else {
        return None;
    };
    let (i, numpad) = match ROW.iter().position(|&c| c == code) {
        Some(i) => (i, false),
        None => (NUMPAD.iter().position(|&c| c == code)?, true),
    };
    let digit = char::from_digit(i as u32, 10)?;
    let typed =
        matches!(modified_key.as_ref(), keyboard::Key::Character(c) if c.chars().eq([digit]));
    (typed || !numpad && !modifiers.shift()).then_some(digit)
}

impl Widget<Message, Theme, Renderer> for Scope<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let child = (self.content.as_widget_mut()).layout(&mut tree.children[0], renderer, limits);
        layout::Node::with_children(child.size(), vec![child])
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            child(layout),
            cursor,
            viewport,
        );
        if tree.state.downcast_ref::<State>().focused {
            // Around the island, in the gaps next to it.
            let quad = renderer::Quad {
                bounds: layout.bounds().expand(4.0),
                border: Border {
                    color: theme.palette().primary,
                    width: 1.0,
                    radius: 4.0.into(),
                },
                ..renderer::Quad::default()
            };
            renderer.fill_quad(quad, Color::TRANSPARENT);
        }
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        // Always, so focusing something else unfocuses it.
        let state = tree.state.downcast_mut::<State>();
        operation.focusable(Some(&self.id), layout.bounds(), state);
        operation.traverse(&mut |operation| {
            self.content.as_widget_mut().operate(
                &mut tree.children[0],
                child(layout),
                renderer,
                operation,
            );
        });
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerPressed { .. }) => {
                // Remembers a checkbox that might lose focus to the click.
                self.focused_field(tree, child(layout), renderer);
                let state = tree.state.downcast_mut::<State>();
                let focused = self.click_focus && cursor.is_over(layout.bounds());
                if state.focused != focused {
                    state.focused = focused;
                    shell.request_redraw();
                }
            }
            Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                modified_key,
                physical_key,
                modifiers,
                ..
            }) => {
                // A named key the modifiers turned into a character (like
                // a numpad arrow with Num Lock on) is that character.
                let key = match (key, modified_key) {
                    (keyboard::Key::Named(_), keyboard::Key::Character(_)) => modified_key,
                    _ => key,
                };
                let digit = digit(modified_key, *physical_key, *modifiers);
                // Before any key moves focus away from it.
                let field = self.focused_field(tree, child(layout), renderer);
                let action = self.key_action(tree, field, key, *physical_key, digit, *modifiers);
                if let Some(action) = action {
                    if let Some(message) = action.send {
                        shell.publish(message);
                    }
                    if let Some(focus) = action.focus {
                        let (id, whole) = match focus {
                            Focus::Field(id) => (id, false),
                            // Unfocuses all fields.
                            Focus::Whole => (self.id.clone(), true),
                        };
                        let mut focus = focusable::focus::<()>(id);
                        (self.content.as_widget_mut()).operate(
                            &mut tree.children[0],
                            child(layout),
                            renderer,
                            &mut focus,
                        );
                        tree.state.downcast_mut::<State>().focused = whole;
                        shell.request_redraw();
                    }
                    // Handled: the island's widgets don't see it.
                    shell.capture_event();
                    return;
                }
            }
            _ => {}
        }
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            child(layout),
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            child(layout),
            cursor,
            viewport,
            renderer,
        )
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            child(layout),
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a> From<Scope<'a>> for Element<'a, Message> {
    fn from(scope: Scope<'a>) -> Self {
        Element::new(scope)
    }
}

/// The layout of the content.
fn child(layout: Layout<'_>) -> Layout<'_> {
    layout.children().next().expect("a scope has one child")
}

/// Finds which of `fields` has focus.
struct FindField<'a> {
    fields: &'a [Field],
    focused: Option<usize>,
}

impl Operation for FindField<'_> {
    fn focusable(&mut self, id: Option<&Id>, _: Rectangle, state: &mut dyn Focusable) {
        if state.is_focused()
            && let Some(id) = id
        {
            self.focused = self
                .fields
                .iter()
                .position(|f| f.id == *id)
                .or(self.focused);
        }
    }

    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }
}
