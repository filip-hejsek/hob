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

use crate::Message;

/// Wraps the UI of a live island, and handles its keys before its widgets
/// see them:
/// - Enter in a text field or checkbox focuses the next field; in the last,
///   it submits. Ctrl+Enter does the same, also in a text area (where Enter
///   starts a new line).
/// - In a checkbox: Space toggles it; `y` checks it and `n` unchecks it, and
///   both move on to the next field, or after the last one to the island as
///   a whole; Up and Down focus the previous and next field.
/// - Escape, while the island has focus, cancels.
/// - While the island as a whole has focus, Enter submits, a button's key
///   presses it, and Up (if there are fields) focuses the last field.
///
/// An island without fields can have focus as a whole: like a text field, it
/// gets it when clicked or through iced's focus operations (with its own
/// [`Id`]), and loses it on a click elsewhere. An island with fields gets it
/// only from its last checkbox.
pub(crate) struct Scope<'a> {
    pub content: Element<'a, Message>,
    /// The island's Id, for focus as a whole.
    pub id: Id,
    /// Whether the island has no fields, so a click focuses it as a whole.
    pub click_focus: bool,
    /// The shown fields, in tree order.
    pub fields: Vec<Field>,
    /// The keys of shown buttons, and what pressing them sends.
    pub keys: Vec<(char, Message)>,
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

/// Whether the island has focus as a whole.
#[derive(Default)]
struct State {
    focused: bool,
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
    /// What pressing `key` with `modifiers` does, if anything.
    fn key_action(
        &mut self,
        tree: &mut Tree,
        key: &keyboard::Key,
        modifiers: keyboard::Modifiers,
        layout: Layout<'_>,
        renderer: &Renderer,
    ) -> Option<KeyAction> {
        let plain = modifiers.is_empty();
        if !plain && modifiers != keyboard::Modifiers::COMMAND {
            return None;
        }
        let whole = tree.state.downcast_ref::<State>().focused;
        let field = if self.fields.is_empty() {
            None
        } else {
            let mut find = FindField {
                fields: &self.fields,
                focused: None,
            };
            (self.content.as_widget_mut()).operate(
                &mut tree.children[0],
                layout,
                renderer,
                &mut find,
            );
            find.focused
        };
        let kind = field.map(|i| &self.fields[i].kind);
        let checkbox = match kind {
            Some(FieldKind::Checkbox {
                checked,
                check,
                uncheck,
            }) => Some((*checked, check, uncheck)),
            _ => None,
        };
        // The field after the focused one, or else the island as a whole.
        let next = |field: usize| match self.fields.get(field + 1) {
            Some(next) => Focus::Field(next.id.clone()),
            None => Focus::Whole,
        };
        match key.as_ref() {
            keyboard::Key::Named(key::Named::Escape) if plain && (whole || field.is_some()) => {
                self.cancel.clone().map(KeyAction::send)
            }
            keyboard::Key::Named(key::Named::Enter) if whole => {
                self.submit.clone().map(KeyAction::send)
            }
            keyboard::Key::Named(key::Named::Enter) => {
                let field = field?;
                if plain && matches!(kind, Some(FieldKind::TextArea)) {
                    return None;
                }
                match self.fields.get(field + 1) {
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
            keyboard::Key::Named(key::Named::ArrowUp) if plain => {
                let previous = match field {
                    _ if whole && !self.click_focus => self.fields.last()?,
                    Some(field) if checkbox.is_some() => self.fields.get(field.checked_sub(1)?)?,
                    _ => return None,
                };
                Some(KeyAction::focus(Focus::Field(previous.id.clone())))
            }
            keyboard::Key::Named(key::Named::ArrowDown) if plain && checkbox.is_some() => {
                Some(KeyAction::focus(next(field?)))
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
                let (_, message) = self.keys.iter().find(|(k, _)| *k == c)?;
                Some(KeyAction::send(message.clone()))
            }
            _ => None,
        }
    }
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
                let state = tree.state.downcast_mut::<State>();
                let focused = self.click_focus && cursor.is_over(layout.bounds());
                if state.focused != focused {
                    state.focused = focused;
                    shell.request_redraw();
                }
            }
            Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                if let Some(action) =
                    self.key_action(tree, key, *modifiers, child(layout), renderer)
                {
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
