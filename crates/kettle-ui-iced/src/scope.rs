//! [`Scope`]: wraps a live island, and handles its keys.

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
/// see them:
/// - Enter in a text field focuses the next field; in the last, it submits.
///   Ctrl+Enter does the same, also in a text area (where Enter starts a new
///   line). In a checkbox, Enter skips the checkboxes after it: it focuses
///   the next field that isn't one, or submits if there's none.
/// - In a checkbox: Space toggles it; `y` checks it and `n` unchecks it, and
///   both move on to the next field, or after the last one to the island as
///   a whole.
/// - In a text field or checkbox (not a text area), Up and Down focus the
///   previous and next field; Down from the last one focuses the island as
///   a whole. Ctrl+Up and Ctrl+Down do the same, also in a text area (where
///   Up and Down move between lines); in a checkbox, they skip the
///   checkboxes before or after it (Ctrl+Up does nothing if there's no
///   other field before them). When they land on a run of checkboxes,
///   they focus the one in it that last had focus, if any.
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

/// A shown field: a text field or area, or a checkbox.
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
}

impl FieldKind {
    fn is_checkbox(&self) -> bool {
        matches!(self, FieldKind::Checkbox { .. })
    }
}

/// Whether the island has focus as a whole, and which checkboxes had
/// focus last.
#[derive(Default)]
struct State {
    focused: bool,
    /// The checkbox that last had focus in each run of checkboxes, oldest
    /// first. Runs can change as fields are shown and hidden, so a run may
    /// hold more than one; the newest counts.
    last_checkboxes: Vec<Id>,
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
    /// Which field has focus, if any; if it's a checkbox, remembers it as
    /// the last one with focus in its run.
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
        if self.fields[field].kind.is_checkbox() {
            let run = self.run(field);
            let state = tree.state.downcast_mut::<State>();
            let ids = &self.fields[run];
            state
                .last_checkboxes
                .retain(|id| !ids.iter().any(|f| f.id == *id));
            state.last_checkboxes.push(self.fields[field].id.clone());
        }
        Some(field)
    }

    /// The run of checkboxes that the field at `field` is in (just that
    /// field if it's not a checkbox).
    fn run(&self, field: usize) -> std::ops::Range<usize> {
        let is_checkbox = |f: &Field| f.kind.is_checkbox();
        let start = self.fields[..field]
            .iter()
            .rposition(|f| !is_checkbox(f))
            .map_or(0, |i| i + 1);
        let end = self.fields[field + 1..]
            .iter()
            .position(|f| !is_checkbox(f))
            .map_or(self.fields.len(), |i| field + 1 + i);
        start..end
    }

    /// Where Ctrl+Up or Ctrl+Down landing on the field at `field` focuses:
    /// in a run of checkboxes, the one that last had focus.
    fn land(&self, state: &State, field: usize) -> Focus {
        let fields = &self.fields[self.run(field)];
        let last =
            (state.last_checkboxes.iter().rev()).find(|id| fields.iter().any(|f| f.id == **id));
        Focus::Field(last.unwrap_or(&self.fields[field].id).clone())
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
        // There, Up and Down move between lines, and Ctrl+Up and Ctrl+Down
        // between fields.
        let in_text_area = matches!(kind, Some(FieldKind::TextArea));
        let arrows_move_focus = !(plain && in_text_area);
        // The field after the focused one, or else the island as a whole.
        let next = |field: usize| match self.fields.get(field + 1) {
            Some(next) => Focus::Field(next.id.clone()),
            None => Focus::Whole,
        };
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
                // From a checkbox, skip the checkboxes after it.
                let next = self.fields[field + 1..]
                    .iter()
                    .find(|f| checkbox.is_none() || !f.kind.is_checkbox());
                match next {
                    Some(next) => Some(KeyAction::focus(Focus::Field(next.id.clone()))),
                    None => self.submit.clone().map(KeyAction::send),
                }
            }
            keyboard::Key::Named(key::Named::Space) if plain => {
                let (checked, check, uncheck) = checkbox?;
                Some(KeyAction::send(
                    if checked { uncheck } else { check }.clone(),
                ))
            }
            // From a checkbox, Ctrl+Up skips the checkboxes before it.
            keyboard::Key::Named(key::Named::ArrowUp) if !plain && checkbox.is_some() => {
                let previous = self.fields[..field?]
                    .iter()
                    .rfind(|f| !f.kind.is_checkbox())?;
                Some(KeyAction::focus(Focus::Field(previous.id.clone())))
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
            // From a checkbox, Ctrl+Down skips the checkboxes after it.
            keyboard::Key::Named(key::Named::ArrowDown) if !plain && checkbox.is_some() => {
                let next = self.fields[field? + 1..]
                    .iter()
                    .find(|f| !f.kind.is_checkbox());
                Some(KeyAction::focus(match next {
                    Some(next) => Focus::Field(next.id.clone()),
                    None => Focus::Whole,
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
            keyboard::Key::Character(c) if plain && (whole || checkbox.is_some()) => {
                let mut chars = c.chars().map(|c| c.to_ascii_lowercase());
                let (Some(c), None) = (chars.next(), chars.next()) else {
                    return None;
                };
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

    /// Presses the button with `key`, if there's one.
    fn press(&self, key: ButtonKey) -> Option<KeyAction> {
        let (_, message) = self.keys.iter().find(|(k, _)| *k == key)?;
        Some(KeyAction::send(message.clone()))
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
