//! A vertical scrollable, as tall as its content if there's room, that
//! keeps a widget in view: the container with a given id (if there is
//! one), whenever it moves or the view's height changes. Its content is
//! rows of the default text, so it shows whole rows, unless it's given less
//! than one.
//!
//! Like the scrollback's [`Sticky`](crate::sticky), it scrolls while it's
//! laid out, so what it shows is right as soon as it's drawn.

use iced::advanced::layout::{self, Layout};
use iced::advanced::text::Renderer as _;
use iced::advanced::widget::operation::Scrollable as ScrollableState;
use iced::advanced::widget::{Id, Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, mouse, overlay, renderer};
use iced::widget::scrollable::{Direction, Scrollbar};
use iced::widget::text::LineHeight;
use iced::widget::{Scrollable, scrollable};
use iced::{Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector};

use crate::prompt::Reveal;

/// A scrollable that keeps a widget in view; see the module docs.
pub(crate) struct Follow<'a, Message> {
    scrollable: Scrollable<'a, Message>,
    target: Id,
    /// The space between rows.
    spacing: f32,
}

/// Creates a [`Follow`] showing `content`, rows `spacing` apart, keeping
/// the container with id `target` in view.
pub(crate) fn follow<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
    target: Id,
    spacing: f32,
) -> Follow<'a, Message> {
    let scrollable = scrollable(content)
        .width(Length::Fill)
        .direction(Direction::Vertical(Scrollbar::new().spacing(4.0)));
    Follow {
        scrollable,
        target,
        spacing,
    }
}

/// The height of as many whole rows, `line` high and `spacing` apart, as
/// fit in `height`; `height` itself if not even one does.
fn whole_rows(height: f32, line: f32, spacing: f32) -> f32 {
    let rows = ((height + spacing) / (line + spacing)).floor();
    match rows < 1.0 {
        true => height,
        false => rows * (line + spacing) - spacing,
    }
}

/// Where the target was, in the content, and the view's height, when last
/// kept in view.
#[derive(Default, PartialEq)]
struct Seen {
    target: Option<Rectangle>,
    height: f32,
}

impl<Message> Widget<Message, Theme, Renderer> for Follow<'_, Message> {
    fn size(&self) -> Size<Length> {
        self.scrollable.size()
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Seen>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Seen::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.scrollable as &dyn Widget<_, _, _>)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&[&self.scrollable as &dyn Widget<_, _, _>]);
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let line = LineHeight::default().to_absolute(renderer.default_size()).0;
        let max = limits.max();
        // Rounded up a little, so that rounding doesn't lose a row.
        let height = whole_rows(max.height + 0.01, line, self.spacing).min(max.height);
        let limits = layout::Limits::new(limits.min(), Size::new(max.width, height));
        let node = (self.scrollable).layout(&mut tree.children[0], renderer, &limits);
        let mut find = Find {
            target: &self.target,
            content: None,
            found: None,
        };
        let layout = Layout::new(&node);
        (self.scrollable).operate(&mut tree.children[0], layout, renderer, &mut find);
        let seen = Seen {
            target: find
                .found
                .zip(find.content)
                .map(|(found, content)| Rectangle {
                    y: found.y - content.y,
                    ..found
                }),
            height: node.size().height,
        };
        let state = tree.state.downcast_mut::<Seen>();
        if *state != seen {
            if let Some(target) = seen.target {
                let mut reveal = Reveal {
                    top: target.y,
                    bottom: target.y + target.height,
                    margin: 0.0,
                };
                (self.scrollable).operate(&mut tree.children[0], layout, renderer, &mut reveal);
            }
            *tree.state.downcast_mut::<Seen>() = seen;
        }
        node
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
        (self.scrollable).draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        (self.scrollable).operate(&mut tree.children[0], layout, renderer, operation);
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
        (self.scrollable).update(
            &mut tree.children[0],
            event,
            layout,
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
        (self.scrollable).mouse_interaction(&tree.children[0], layout, cursor, viewport, renderer)
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        (self.scrollable).overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message: 'a> From<Follow<'a, Message>> for Element<'a, Message> {
    fn from(follow: Follow<'a, Message>) -> Self {
        Element::new(follow)
    }
}

/// Finds the outermost scrollable's content and the container with the
/// target id.
struct Find<'a> {
    target: &'a Id,
    content: Option<Rectangle>,
    found: Option<Rectangle>,
}

impl Operation for Find<'_> {
    fn scrollable(
        &mut self,
        _: Option<&Id>,
        _: Rectangle,
        content: Rectangle,
        _: Vector,
        _: &mut dyn ScrollableState,
    ) {
        self.content.get_or_insert(content);
    }

    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        if id == Some(self.target) {
            self.found = Some(bounds);
        }
    }

    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }
}

#[cfg(test)]
mod tests {
    use super::whole_rows;

    #[test]
    fn whole_rows_fit() {
        assert_eq!(whole_rows(25.0, 20.0, 10.0), 20.0);
        assert_eq!(whole_rows(49.0, 20.0, 10.0), 20.0);
        assert_eq!(whole_rows(50.0, 20.0, 10.0), 50.0);
        assert_eq!(whole_rows(15.0, 20.0, 10.0), 15.0);
    }
}
