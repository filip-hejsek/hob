//! [`Choice`]: iced's checkbox or radio button, made focusable, and
//! [`RadioGroup`], which lays out a group of radio buttons.

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::{self, Renderer as _};
use iced::advanced::text::Renderer as _;
use iced::advanced::widget::operation::Focusable;
use iced::advanced::widget::{Id, Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, mouse};
use iced::widget::{checkbox, radio, text};
use iced::{Border, Color, Element, Event, Length, Rectangle, Renderer, Size, Theme, touch};

use crate::Message;

/// Shown after a checkbox's label while it has focus.
const CHECKBOX_HINT: &str = "[y/n]";

/// The least width, in ems, left for the checkbox or radio button and its
/// label when room is kept for the hint.
const MIN_WIDTH: f32 = 6.0;

/// How far the focus border is outside the checkbox and its label.
const RING: f32 = 3.0;

/// A checkbox or radio button that can have focus, like a text field: it
/// gets it when clicked or through iced's focus operations, and loses it on
/// a click elsewhere. While focused, it has a thin border, and a muted hint
/// follows its label (for a radio button, while any button of its group has
/// focus); the hint's space is always kept, so nothing moves. Its keys are
/// handled by the island's [`Scope`](crate::scope::Scope).
pub(crate) struct Choice<'a> {
    inner: Element<'a, Message>,
    hint: Option<Element<'a, Message>>,
    id: Option<Id>,
    interactive: bool,
}

/// Whether the checkbox or radio button has focus.
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

impl<'a> Choice<'a> {
    /// A checkbox. Without `on_toggle`, it's disabled and can't get focus.
    pub(crate) fn checkbox(
        label: &'a str,
        checked: bool,
        on_toggle: Option<impl Fn(bool) -> Message + 'a>,
    ) -> Self {
        Choice {
            interactive: on_toggle.is_some(),
            inner: checkbox(checked)
                .label(label)
                .text_wrapping(text::Wrapping::WordOrGlyph)
                .on_toggle_maybe(on_toggle)
                .into(),
            hint: Some(hint(CHECKBOX_HINT)),
            id: None,
        }
    }

    /// A radio button, which sends `select` when clicked, and shows `key` as
    /// its hint. Unless `interactive`, it looks disabled, ignores clicks, and
    /// can't get focus.
    pub(crate) fn radio(
        label: &'a str,
        selected: bool,
        select: Message,
        key: Option<char>,
        interactive: bool,
    ) -> Self {
        let radio = radio(label, (), selected.then_some(()), move |()| select)
            .text_wrapping(text::Wrapping::WordOrGlyph);
        let radio = if interactive {
            radio
        } else {
            radio.style(disabled_radio)
        };
        Choice {
            interactive,
            inner: radio.into(),
            hint: key.map(|key| hint(format!("[{key}]"))),
            id: None,
        }
    }

    pub(crate) fn id(mut self, id: Id) -> Self {
        self.id = Some(id);
        self
    }

    /// Draws it, with the hint if `show_hint`.
    #[allow(clippy::too_many_arguments)]
    fn draw_with_hint(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        show_hint: bool,
    ) {
        let mut children = layout.children();
        let inner = children.next().unwrap();
        self.inner.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            inner,
            cursor,
            viewport,
        );
        if is_focused(tree) {
            // Like the island's focus border, but within the gaps around it.
            let quad = renderer::Quad {
                bounds: inner.bounds().expand(RING),
                border: Border {
                    color: theme.palette().primary,
                    width: 1.0,
                    radius: 3.0.into(),
                },
                ..renderer::Quad::default()
            };
            renderer.fill_quad(quad, Color::TRANSPARENT);
        }
        if show_hint && let (Some(hint), Some(layout)) = (&self.hint, children.next()) {
            hint.as_widget().draw(
                &tree.children[1],
                renderer,
                theme,
                style,
                layout,
                cursor,
                viewport,
            );
        }
    }
}

/// A muted hint.
fn hint<'a>(s: impl text::IntoFragment<'a>) -> Element<'a, Message> {
    text(s).style(text::secondary).into()
}

/// Whether the [`Choice`] with this tree has focus.
fn is_focused(tree: &Tree) -> bool {
    tree.state.downcast_ref::<State>().focused
}

/// A radio button that looks disabled, like a disabled checkbox.
fn disabled_radio(theme: &Theme, status: radio::Status) -> radio::Style {
    let palette = theme.extended_palette();
    let (radio::Status::Active { is_selected } | radio::Status::Hovered { is_selected }) = status;
    radio::Style {
        background: palette.background.weaker.color.into(),
        dot_color: palette.background.strong.color,
        border_width: 1.0,
        border_color: if is_selected {
            palette.background.strong.color
        } else {
            palette.background.weak.color
        },
        text_color: None,
    }
}

impl Widget<Message, Theme, Renderer> for Choice<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        let mut children = vec![Tree::new(&self.inner)];
        children.extend(self.hint.as_ref().map(Tree::new));
        children
    }

    fn diff(&self, tree: &mut Tree) {
        let mut children = vec![&self.inner];
        children.extend(&self.hint);
        tree.diff_children(&children);
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
        let hint = self.hint.as_mut().map(|hint| {
            hint.as_widget_mut().layout(
                &mut tree.children[1],
                renderer,
                &layout::Limits::new(Size::ZERO, Size::new(f32::INFINITY, f32::INFINITY)),
            )
        });
        // About a space's width (as in button labels, "Yes [y]") outside
        // the focus border.
        let gap = RING + 0.25 * renderer.default_size().0;
        // The label wraps to leave room for the hint, unless that leaves it
        // too little; then the hint may go past the edge.
        let room = hint.as_ref().map_or(0.0, |hint| gap + hint.size().width);
        let room = if limits.max().width - room >= MIN_WIDTH * renderer.default_size().0 {
            room
        } else {
            0.0
        };
        let inner = (self.inner.as_widget_mut()).layout(
            &mut tree.children[0],
            renderer,
            &limits.shrink(Size::new(room, 0.0)),
        );
        let size = inner.size();
        let Some(hint) = hint else {
            return layout::Node::with_children(
                limits.resolve(Length::Shrink, Length::Shrink, size),
                vec![inner],
            );
        };
        let hint_size = hint.size();
        let hint = hint.move_to((size.width + gap, (size.height - hint_size.height) / 2.0));
        // The hint's space is kept when it's hidden, so nothing moves.
        let size = Size::new(
            size.width + gap + hint_size.width,
            size.height.max(hint_size.height),
        );
        layout::Node::with_children(
            limits.resolve(Length::Shrink, Length::Shrink, size),
            vec![inner, hint],
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
        let focused = is_focused(tree);
        self.draw_with_hint(
            tree, renderer, theme, style, layout, cursor, viewport, focused,
        );
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
        // A disabled checkbox ignores clicks itself; a radio button doesn't.
        if self.interactive {
            self.inner.as_widget_mut().update(
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
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        if !self.interactive {
            return mouse::Interaction::default();
        }
        self.inner.as_widget().mouse_interaction(
            &tree.children[0],
            inner(layout),
            cursor,
            viewport,
            renderer,
        )
    }
}

impl<'a> From<Choice<'a>> for Element<'a, Message> {
    fn from(choice: Choice<'a>) -> Self {
        Element::new(choice)
    }
}

/// The layout of the checkbox or radio button itself.
fn inner(layout: Layout<'_>) -> Layout<'_> {
    layout.children().next().expect("a choice has children")
}

/// The radio buttons of a radio group, in a column, which all show their
/// hints while any of them has focus.
pub(crate) struct RadioGroup<'a> {
    options: Vec<Choice<'a>>,
    spacing: f32,
}

impl<'a> RadioGroup<'a> {
    pub(crate) fn new(options: Vec<Choice<'a>>, spacing: f32) -> Self {
        RadioGroup { options, spacing }
    }
}

impl Widget<Message, Theme, Renderer> for RadioGroup<'_> {
    fn children(&self) -> Vec<Tree> {
        (self.options.iter())
            .map(|o| Tree::new(o as &dyn Widget<_, _, _>))
            .collect()
    }

    fn diff(&self, tree: &mut Tree) {
        let options: Vec<_> = (self.options.iter())
            .map(|o| o as &dyn Widget<_, _, _>)
            .collect();
        tree.diff_children(&options);
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
        let mut y = 0.0;
        let mut width: f32 = 0.0;
        let children = (self.options.iter_mut())
            .zip(&mut tree.children)
            .enumerate()
            .map(|(i, (option, tree))| {
                if i > 0 {
                    y += self.spacing;
                }
                let node = option.layout(tree, renderer, limits).move_to((0.0, y));
                y += node.size().height;
                width = width.max(node.size().width);
                node
            })
            .collect();
        layout::Node::with_children(
            limits.resolve(Length::Shrink, Length::Shrink, Size::new(width, y)),
            children,
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
        let show_hints = tree.children.iter().any(is_focused);
        for ((option, tree), layout) in self
            .options
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
        {
            option.draw_with_hint(
                tree, renderer, theme, style, layout, cursor, viewport, show_hints,
            );
        }
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        for ((option, tree), layout) in (self.options.iter_mut())
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            option.operate(tree, layout, renderer, operation);
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
        for ((option, tree), layout) in (self.options.iter_mut())
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            option.update(
                tree, event, layout, cursor, renderer, clipboard, shell, viewport,
            );
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        (self.options.iter())
            .zip(&tree.children)
            .zip(layout.children())
            .map(|((option, tree), layout)| {
                option.mouse_interaction(tree, layout, cursor, viewport, renderer)
            })
            .max()
            .unwrap_or_default()
    }
}

impl<'a> From<RadioGroup<'a>> for Element<'a, Message> {
    fn from(group: RadioGroup<'a>) -> Self {
        Element::new(group)
    }
}
