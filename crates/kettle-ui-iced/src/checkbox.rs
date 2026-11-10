//! [`Checkbox`]: iced's checkbox, made focusable.

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::{self, Renderer as _};
use iced::advanced::text::Renderer as _;
use iced::advanced::widget::operation::Focusable;
use iced::advanced::widget::{Id, Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, mouse};
use iced::widget::{checkbox, text};
use iced::{Border, Color, Element, Event, Length, Rectangle, Renderer, Size, Theme, touch};

use crate::Message;

/// Shown after the label while the checkbox has focus.
const HINT: &str = "[y/n]";

/// The least width, in ems, left for the checkbox and its label when
/// room is kept for the [`HINT`].
const MIN_WIDTH: f32 = 6.0;

/// How far the focus border is outside the checkbox and its label.
const RING: f32 = 3.0;

/// A checkbox that can have focus, like a text field: it gets it when
/// clicked or through iced's focus operations, and loses it on a click
/// elsewhere. While focused, it has a thin border, and a muted [`HINT`]
/// follows its label; the hint's space is always kept, so nothing moves.
/// Its keys are handled by the island's [`Scope`](crate::scope::Scope).
pub(crate) struct Checkbox<'a> {
    checkbox: Element<'a, Message>,
    hint: Element<'a, Message>,
    id: Option<Id>,
    interactive: bool,
}

/// Whether the checkbox has focus.
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

impl<'a> Checkbox<'a> {
    /// Without `on_toggle`, the checkbox is disabled and can't get focus.
    pub(crate) fn new(
        label: &'a str,
        checked: bool,
        on_toggle: Option<impl Fn(bool) -> Message + 'a>,
    ) -> Self {
        Checkbox {
            interactive: on_toggle.is_some(),
            checkbox: checkbox(checked)
                .label(label)
                .text_wrapping(text::Wrapping::WordOrGlyph)
                .on_toggle_maybe(on_toggle)
                .into(),
            hint: text(HINT).style(text::secondary).into(),
            id: None,
        }
    }

    pub(crate) fn id(mut self, id: Id) -> Self {
        self.id = Some(id);
        self
    }
}

impl Widget<Message, Theme, Renderer> for Checkbox<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.checkbox), Tree::new(&self.hint)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&[&self.checkbox, &self.hint]);
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Shrink, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let hint = (self.hint.as_widget_mut()).layout(
            &mut tree.children[1],
            renderer,
            &layout::Limits::new(Size::ZERO, Size::new(f32::INFINITY, f32::INFINITY)),
        );
        // About a space's width (as in button labels, "Yes [y]") outside
        // the focus border.
        let gap = RING + 0.25 * renderer.default_size().0;
        // The label wraps to leave room for the hint, unless that leaves it
        // too little; then the hint may go past the edge.
        let room = gap + hint.size().width;
        let room = if limits.max().width - room >= MIN_WIDTH * renderer.default_size().0 {
            room
        } else {
            0.0
        };
        let checkbox = (self.checkbox.as_widget_mut()).layout(
            &mut tree.children[0],
            renderer,
            &limits.shrink(Size::new(room, 0.0)),
        );
        let (size, hint_size) = (checkbox.size(), hint.size());
        let hint = hint.move_to((size.width + gap, (size.height - hint_size.height) / 2.0));
        // The hint's space is kept when it's hidden, so nothing moves.
        let size = Size::new(
            size.width + gap + hint_size.width,
            size.height.max(hint_size.height),
        );
        layout::Node::with_children(
            limits.resolve(Length::Shrink, Length::Shrink, size),
            vec![checkbox, hint],
        )
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
        let mut children = layout.children();
        let (checkbox, hint) = (children.next().unwrap(), children.next().unwrap());
        self.checkbox.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            checkbox,
            cursor,
            viewport,
        );
        if tree.state.downcast_ref::<State>().focused {
            // Like the island's focus border, but within the gaps around it.
            let quad = renderer::Quad {
                bounds: checkbox.bounds().expand(RING),
                border: Border {
                    color: theme.palette().primary,
                    width: 1.0,
                    radius: 3.0.into(),
                },
                ..renderer::Quad::default()
            };
            renderer.fill_quad(quad, Color::TRANSPARENT);
            self.hint.as_widget().draw(
                &tree.children[1],
                renderer,
                theme,
                style,
                hint,
                cursor,
                viewport,
            );
        }
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        _renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        if let Some(id) = &self.id {
            let state = tree.state.downcast_mut::<State>();
            operation.focusable(Some(id), inner(layout).bounds(), state);
        }
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
        let state = tree.state.downcast_mut::<State>();
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerPressed { .. }) => {
                // Not in the hint's space, where a click doesn't toggle it.
                let focused = self.interactive && cursor.is_over(inner(layout).bounds());
                if state.focused != focused {
                    state.focused = focused;
                    shell.request_redraw();
                }
            }
            _ => {}
        }
        self.checkbox.as_widget_mut().update(
            &mut tree.children[0],
            event,
            inner(layout),
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
        self.checkbox.as_widget().mouse_interaction(
            &tree.children[0],
            inner(layout),
            cursor,
            viewport,
            renderer,
        )
    }
}

impl<'a> From<Checkbox<'a>> for Element<'a, Message> {
    fn from(checkbox: Checkbox<'a>) -> Self {
        Element::new(checkbox)
    }
}

/// The layout of the checkbox itself.
fn inner(layout: Layout<'_>) -> Layout<'_> {
    layout.children().next().expect("a checkbox has children")
}
