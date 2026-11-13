//! A vertical scrollable that keeps what it shows in place when its content
//! changes.
//!
//! What's at the top of the view stays there: the view's top is anchored to
//! the innermost widget there (by its path in the layout), so content that
//! changes or reflows above it doesn't move it. Within a widget that reflows
//! to another height (a paragraph), the anchor moves proportionally, by whole
//! lines.
//!
//! While it's scrolled to the bottom, it also keeps the content's bottom in
//! sight, so new content pushes what's shown up. Content that gets shorter
//! doesn't pull what's shown down: the content is padded to the bottom of the
//! view, so there's empty space below it instead. The padding goes away as the
//! view moves up, or when the owner asks to reveal the bottom, and it doesn't
//! grow when the view's width changes.
//!
//! iced's scrollable only keeps its offset from its top or its bottom, so this
//! sets the offset each time the content is laid out, before it's drawn.

use crate::keyed::KeyedState;

use std::mem;

use iced::advanced::layout::{self, Layout};
use iced::advanced::text::Renderer as _;
use iced::advanced::widget::operation::Scrollable as ScrollableState;
use iced::advanced::widget::{Id, Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, mouse, overlay, renderer};
use iced::widget::scrollable;
use iced::widget::scrollable::AbsoluteOffset;
use iced::widget::text::LineHeight;
use iced::{Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector};

/// A scrollable that keeps what it shows in place; see the module docs.
pub(crate) struct Sticky<'a, Message> {
    scrollable: Element<'a, Message>,
    reveal: u64,
}

/// Creates a [`Sticky`] scrollable showing `content`, filling the space it's
/// given. It scrolls to the bottom, and drops the empty space there, whenever
/// `reveal` changes.
pub(crate) fn sticky<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
    id: Id,
    reveal: u64,
) -> Sticky<'a, Message> {
    let floor = Floor {
        content: content.into(),
    };
    let scrollable = scrollable(floor)
        .id(id)
        .width(Length::Fill)
        .height(Length::Fill);
    Sticky {
        scrollable: scrollable.into(),
        reveal,
    }
}

/// What was shown when last seen.
#[derive(Clone, Debug)]
struct Seen {
    /// The top of the view, in the content's coordinates.
    top: f32,
    /// The height of the view.
    height: f32,
    /// The width the view was laid out with.
    width: f32,
    /// The content's height, without padding.
    content: f32,
    /// Whether the view was at the bottom of the content.
    at_bottom: bool,
    /// The last `reveal` handled.
    reveal: u64,
    /// What's at the top of the view.
    anchor: Option<Anchor>,
    /// Whether the content was diffed since it was last laid out, so its
    /// [`Keyed`](crate::keyed::Keyed) columns say how their children moved.
    diffed: bool,
}

/// A point in the content, by the innermost layout node it's in.
#[derive(Clone, Debug)]
struct Anchor {
    /// The node's path from the content's node, by child index.
    path: Vec<usize>,
    /// The point's offset from the node's top. Negative if the point is
    /// above the node (in space before it).
    offset: f32,
    /// The node's height.
    height: f32,
}

impl<Message> Widget<Message, Theme, Renderer> for Sticky<'_, Message> {
    fn size(&self) -> Size<Length> {
        self.scrollable.as_widget().size()
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Seen>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Seen {
            top: 0.0,
            height: 0.0,
            width: 0.0,
            content: 0.0,
            at_bottom: true,
            reveal: self.reveal,
            anchor: None,
            diffed: false,
        })
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.scrollable)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.scrollable));
        tree.state.downcast_mut::<Seen>().diffed = true;
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let mut seen = tree.state.downcast_mut::<Seen>().clone();
        let reveal = seen.reveal != self.reveal;
        if reveal {
            seen.reveal = self.reveal;
            seen.at_bottom = true;
        }
        let width = limits.max().width;
        // The floor places the view, as it needs the content laid out.
        floor_state(tree).plan = Plan {
            anchor: seen.anchor.take(),
            top: seen.top,
            height: limits.max().height,
            at_bottom: seen.at_bottom,
            reveal,
            remap: mem::take(&mut seen.diffed),
            // The empty space shown at the bottom.
            resized: (width != seen.width)
                .then(|| (seen.top + seen.height - seen.content).max(0.0)),
        };
        seen.width = width;

        let node =
            (self.scrollable.as_widget_mut()).layout(&mut tree.children[0], renderer, limits);

        let floor = floor_state(tree);
        let top = floor.top;
        seen.content = floor.content;
        let mut scroll = ScrollTo(top);
        (self.scrollable.as_widget_mut()).operate(
            &mut tree.children[0],
            Layout::new(&node),
            renderer,
            &mut scroll,
        );
        // Read back, as iced keeps it within the content.
        let mut look = Look(None);
        (self.scrollable.as_widget_mut()).operate(
            &mut tree.children[0],
            Layout::new(&node),
            renderer,
            &mut look,
        );
        if let Some(view) = look.0 {
            seen.top = view.top;
            seen.height = view.height;
            seen.at_bottom = view.at_bottom;
        }
        seen.anchor = Some(anchor(content(Layout::new(&node).child(0)), seen.top));
        *tree.state.downcast_mut::<Seen>() = seen;
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
        (self.scrollable.as_widget()).draw(
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
        (self.scrollable.as_widget_mut()).operate(
            &mut tree.children[0],
            layout,
            renderer,
            operation,
        );
        // The operation may have scrolled.
        self.look(tree, layout, renderer);
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
        (self.scrollable.as_widget_mut()).update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
        // The event may have scrolled.
        self.look(tree, layout, renderer);
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        (self.scrollable.as_widget()).mouse_interaction(
            &tree.children[0],
            layout,
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
        (self.scrollable.as_widget_mut()).overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<Message> Sticky<'_, Message> {
    /// Updates what was seen from the scrollable as it is now.
    fn look(&mut self, tree: &mut Tree, layout: Layout<'_>, renderer: &Renderer) {
        let mut look = Look(None);
        (self.scrollable.as_widget_mut()).operate(
            &mut tree.children[0],
            layout,
            renderer,
            &mut look,
        );
        if let Some(view) = look.0 {
            let seen = tree.state.downcast_mut::<Seen>();
            if seen.top != view.top {
                seen.anchor = Some(anchor(content(layout.child(0)), view.top));
            }
            seen.top = view.top;
            seen.height = view.height;
            seen.at_bottom = view.at_bottom;
        }
    }
}

impl<'a, Message: 'a> From<Sticky<'a, Message>> for Element<'a, Message> {
    fn from(sticky: Sticky<'a, Message>) -> Self {
        Element::new(sticky)
    }
}

/// The [`Floor`]'s state, inside the scrollable's.
fn floor_state(tree: &mut Tree) -> &mut FloorState {
    tree.children[0].children[0]
        .state
        .downcast_mut::<FloorState>()
}

/// What a scrollable shows.
#[derive(Clone, Copy, Debug)]
struct View {
    top: f32,
    height: f32,
    at_bottom: bool,
}

/// Reads what the outermost scrollable shows, without going into its
/// content.
struct Look(Option<View>);

impl Operation for Look {
    fn scrollable(
        &mut self,
        _: Option<&Id>,
        bounds: Rectangle,
        content_bounds: Rectangle,
        translation: Vector,
        _: &mut dyn ScrollableState,
    ) {
        if self.0.is_none() {
            let bottom = translation.y + bounds.height;
            self.0 = Some(View {
                top: translation.y,
                height: bounds.height,
                // The translation is rounded.
                at_bottom: bottom >= content_bounds.height - 1.0,
            });
        }
    }

    fn traverse(&mut self, _: &mut dyn FnMut(&mut dyn Operation)) {}
}

/// Scrolls the outermost scrollable to the given offset from the top,
/// without going into its content.
struct ScrollTo(f32);

impl Operation for ScrollTo {
    fn scrollable(
        &mut self,
        _: Option<&Id>,
        _: Rectangle,
        _: Rectangle,
        _: Vector,
        state: &mut dyn ScrollableState,
    ) {
        state.scroll_to(AbsoluteOffset {
            x: None,
            y: Some(self.0),
        });
    }

    fn traverse(&mut self, _: &mut dyn FnMut(&mut dyn Operation)) {}
}

/// Content at least as tall as its state says.
struct Floor<'a, Message> {
    content: Element<'a, Message>,
}

/// How a [`Floor`] places the view, and where it did.
struct FloorState {
    /// How to place the view.
    plan: Plan,
    /// Where it placed the view's top.
    top: f32,
    /// The content's height, without padding.
    content: f32,
}

/// How to place the view, from what was seen before.
#[derive(Default)]
struct Plan {
    /// Where the view's top was, in the content as it was.
    anchor: Option<Anchor>,
    /// Where the view's top was, if there's no anchor or it's lost.
    top: f32,
    /// The view's height.
    height: f32,
    /// Whether the view was at the bottom.
    at_bottom: bool,
    /// Whether to reveal the bottom, without empty space.
    reveal: bool,
    /// Whether the content's keyed columns say how their children moved.
    remap: bool,
    /// If the view's width changed, the empty space shown at the bottom
    /// before, which mustn't grow.
    resized: Option<f32>,
}

impl Plan {
    /// Where to place the view's top, with the content laid out as `node`.
    fn top(&self, node: &layout::Node, tree: &Tree, line: f32) -> f32 {
        let content = node.size().height;
        let height = self.height;
        if self.reveal {
            return (content - height).max(0.0);
        }
        let anchored = self.anchor.as_ref().and_then(|anchor| {
            let remap = self.remap.then_some(tree);
            resolve(node, remap, anchor, self.resized.is_some(), line)
        });
        let mut top = anchored.unwrap_or(self.top);
        if self.at_bottom {
            top = top.max(content - height);
        }
        if let Some(space) = self.resized {
            top = top.min(content + space - height);
        }
        top.max(0.0)
    }
}

/// The anchor for the point `top` (from `layout`'s top) in `layout`: the
/// innermost node it's in. In space between nodes, the nearest node below
/// it.
fn anchor(layout: Layout<'_>, top: f32) -> Anchor {
    let origin = layout.bounds().y;
    let mut path = Vec::new();
    let mut node = layout;
    loop {
        let y = origin + top;
        let mut below: Option<(usize, Layout<'_>)> = None;
        let mut inside = None;
        for (i, child) in node.children().enumerate() {
            let bounds = child.bounds();
            if bounds.y <= y && y < bounds.y + bounds.height {
                inside = Some((i, child));
                break;
            }
            if bounds.y > y && below.is_none_or(|(_, b)| bounds.y < b.bounds().y) {
                below = Some((i, child));
            }
        }
        match (inside, below) {
            (Some((i, child)), _) => {
                path.push(i);
                node = child;
            }
            (None, Some((i, child))) => {
                path.push(i);
                node = child;
                break;
            }
            (None, None) => break,
        }
    }
    let bounds = node.bounds();
    Anchor {
        path,
        offset: origin + top - bounds.y,
        height: bounds.height,
    }
}

/// Where `anchor` is in the content laid out as `node`, or `None` if its
/// node is gone. With `tree` (the content's), keyed columns in it map the
/// path's indices to where their children moved. If `reflowed`, a point
/// inside a node that got another height moves proportionally, by whole
/// `line`s.
fn resolve(
    node: &layout::Node,
    mut tree: Option<&Tree>,
    anchor: &Anchor,
    reflowed: bool,
    line: f32,
) -> Option<f32> {
    let mut node = node;
    let mut y = 0.0;
    for &i in &anchor.path {
        let moved = tree
            .filter(|tree| tree.tag == tree::Tag::of::<KeyedState>())
            .and_then(|tree| tree.state.downcast_ref::<KeyedState>().moved.as_ref());
        let (i, exact) = match moved {
            // If the child is gone, the next one that isn't, at its top.
            Some(moved) => moved[i.min(moved.len())..]
                .iter()
                .enumerate()
                .find_map(|(n, j)| j.map(|j| (j, n == 0)))?,
            None => (i, true),
        };
        let children = node.children();
        let child = children.get(i)?;
        tree = tree
            .filter(|tree| tree.children.len() == children.len())
            .map(|tree| &tree.children[i]);
        node = child;
        y += node.bounds().y;
        if !exact {
            return Some(y);
        }
    }
    let (offset, old) = (anchor.offset, anchor.height);
    let height = node.size().height;
    let offset = if offset >= old {
        // Below the node, in space after it.
        height + offset - old
    } else if reflowed && offset > 0.0 && height != old {
        (offset * height / old / line).round() * line
    } else {
        offset
    };
    Some(y + offset)
}

impl<Message> Widget<Message, Theme, Renderer> for Floor<'_, Message> {
    fn size(&self) -> Size<Length> {
        Size {
            width: self.content.as_widget().size().width,
            height: Length::Shrink,
        }
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<FloorState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(FloorState {
            plan: Plan::default(),
            top: 0.0,
            content: 0.0,
        })
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let content =
            (self.content.as_widget_mut()).layout(&mut tree.children[0], renderer, limits);
        let line = LineHeight::default().to_absolute(renderer.default_size()).0;
        let state = tree.state.downcast_ref::<FloorState>();
        let top = state.plan.top(&content, &tree.children[0], line);
        let size = content.size();
        let state = tree.state.downcast_mut::<FloorState>();
        state.top = top;
        state.content = size.height;
        // Tall enough for the view to stay there.
        let min = match top > 0.0 {
            true => top + state.plan.height,
            false => 0.0,
        };
        let size = Size::new(size.width, size.height.max(min));
        layout::Node::with_children(size, vec![content])
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
        (self.content.as_widget()).draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            content(layout),
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
        (self.content.as_widget_mut()).operate(
            &mut tree.children[0],
            content(layout),
            renderer,
            operation,
        );
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
        (self.content.as_widget_mut()).update(
            &mut tree.children[0],
            event,
            content(layout),
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
        (self.content.as_widget()).mouse_interaction(
            &tree.children[0],
            content(layout),
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
        (self.content.as_widget_mut()).overlay(
            &mut tree.children[0],
            content(layout),
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message: 'a> From<Floor<'a, Message>> for Element<'a, Message> {
    fn from(floor: Floor<'a, Message>) -> Self {
        Element::new(floor)
    }
}

/// The layout of a [`Floor`]'s content.
fn content(layout: Layout<'_>) -> Layout<'_> {
    layout.children().next().expect("a floor has its content")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyed::keyed;
    use iced::advanced::renderer::Headless;
    use iced::widget::space;
    use iced::{Font, Pixels};
    use iced_test::futures::futures::executor::block_on;
    use iced_test::runtime::user_interface::{Cache, UserInterface};

    /// A [`Sticky`] 100 pixels high (and 100 wide, unless changed), with
    /// content of a given height.
    struct Harness {
        renderer: Renderer,
        cache: Cache,
        reveal: u64,
        width: f32,
    }

    impl Harness {
        fn new() -> Self {
            let renderer = block_on(Renderer::new(Font::DEFAULT, Pixels(16.0), None))
                .expect("a headless renderer");
            Harness {
                renderer,
                cache: Cache::default(),
                reveal: 0,
                width: 100.0,
            }
        }

        /// Lays out content `height` pixels high, then applies `operation`,
        /// and returns the top of the view and the (padded) content's height.
        fn show(&mut self, height: f32, operation: Option<&mut dyn Operation>) -> (f32, f32) {
            self.show_rows(&[(0, height)], operation)
        }

        /// Like [`show`](Self::show), with content of rows with the given
        /// keys and heights.
        fn show_rows(
            &mut self,
            rows: &[(u32, f32)],
            operation: Option<&mut dyn Operation>,
        ) -> (f32, f32) {
            let rows = (rows.iter()).map(|&(key, height)| (key, space().height(height).into()));
            let content = keyed(rows);
            let element: Element<'_, ()> = sticky(content, Id::unique(), self.reveal).into();
            let cache = std::mem::take(&mut self.cache);
            let size = Size::new(self.width, 100.0);
            let mut ui = UserInterface::build(element, size, cache, &mut self.renderer);
            if let Some(operation) = operation {
                ui.operate(&self.renderer, operation);
            }
            let mut read = Read(None);
            ui.operate(&self.renderer, &mut read);
            self.cache = ui.into_cache();
            read.0.expect("a scrollable")
        }

        /// Like [`show`](Self::show), then scrolls down by `y` pixels (up if
        /// negative).
        fn scroll(&mut self, height: f32, y: f32) -> (f32, f32) {
            self.show(height, Some(&mut ScrollBy(y)))
        }
    }

    /// Reads the top of the view and the content's height.
    struct Read(Option<(f32, f32)>);

    impl Operation for Read {
        fn scrollable(
            &mut self,
            _: Option<&Id>,
            _: Rectangle,
            content_bounds: Rectangle,
            translation: Vector,
            _: &mut dyn ScrollableState,
        ) {
            self.0.get_or_insert((translation.y, content_bounds.height));
        }

        fn traverse(&mut self, _: &mut dyn FnMut(&mut dyn Operation)) {}
    }

    struct ScrollBy(f32);

    impl Operation for ScrollBy {
        fn scrollable(
            &mut self,
            _: Option<&Id>,
            bounds: Rectangle,
            content_bounds: Rectangle,
            _: Vector,
            state: &mut dyn ScrollableState,
        ) {
            let offset = AbsoluteOffset { x: 0.0, y: self.0 };
            state.scroll_by(offset, bounds, content_bounds);
        }

        fn traverse(&mut self, _: &mut dyn FnMut(&mut dyn Operation)) {}
    }

    #[test]
    fn starts_at_the_bottom_and_stays_there_as_content_grows() {
        let mut h = Harness::new();
        assert_eq!(h.show(300.0, None), (200.0, 300.0));
        assert_eq!(h.show(400.0, None), (300.0, 400.0));
    }

    #[test]
    fn keeps_the_view_in_place_when_not_at_the_bottom() {
        let mut h = Harness::new();
        h.show(300.0, None);
        assert_eq!(h.scroll(300.0, -150.0), (50.0, 300.0));
        // Growing doesn't move it.
        assert_eq!(h.show(500.0, None), (50.0, 500.0));
        // Neither does shrinking past its bottom: the content is padded.
        assert_eq!(h.show(80.0, None), (50.0, 150.0));
    }

    #[test]
    fn leaves_empty_space_when_content_shrinks_at_the_bottom() {
        let mut h = Harness::new();
        h.show(300.0, None);
        assert_eq!(h.show(250.0, None), (200.0, 300.0));
        // Still at the bottom: growing into the empty space doesn't move the
        // view, growing beyond it does.
        assert_eq!(h.show(280.0, None), (200.0, 300.0));
        assert_eq!(h.show(350.0, None), (250.0, 350.0));
    }

    #[test]
    fn empty_space_goes_as_the_view_moves_up() {
        let mut h = Harness::new();
        h.show(300.0, None);
        h.show(150.0, None);
        assert_eq!(h.scroll(150.0, -120.0), (80.0, 300.0));
        // Only down to the view's bottom.
        assert_eq!(h.show(150.0, None), (80.0, 180.0));
    }

    #[test]
    fn reveal_scrolls_to_the_bottom_without_empty_space() {
        let mut h = Harness::new();
        h.show(300.0, None);
        h.scroll(300.0, -200.0);
        h.show(150.0, None);
        h.reveal += 1;
        assert_eq!(h.show(250.0, None), (150.0, 250.0));
        // At the bottom, the empty space is dropped too.
        h.show(150.0, None);
        h.reveal += 1;
        assert_eq!(h.show(160.0, None), (60.0, 160.0));
    }

    /// The height of a line of the default text.
    const LINE: f32 = 16.0 * 1.3;

    /// Rows of the given heights, keyed by index.
    fn rows(heights: &[f32]) -> Vec<(u32, f32)> {
        (0..).zip(heights.iter().copied()).collect()
    }

    #[test]
    fn keeps_the_row_at_the_top_when_content_above_changes() {
        let mut h = Harness::new();
        h.show_rows(&rows(&[100.0, 100.0, 100.0]), None);
        assert_eq!(
            h.show_rows(&rows(&[100.0, 100.0, 100.0]), Some(&mut ScrollBy(-170.0))),
            (30.0, 300.0)
        );
        // The top is 30 pixels into the first row: that row grows, so its
        // top stays.
        assert_eq!(
            h.show_rows(&rows(&[150.0, 100.0, 100.0]), None),
            (30.0, 350.0)
        );
        h.show_rows(&rows(&[150.0, 100.0, 100.0]), Some(&mut ScrollBy(130.0)));
        // Now it's 10 pixels into the second one.
        assert_eq!(
            h.show_rows(&rows(&[50.0, 100.0, 100.0]), None),
            (60.0, 250.0)
        );
    }

    #[test]
    fn follows_the_row_at_the_top_when_rows_before_it_go() {
        let mut h = Harness::new();
        h.show_rows(&rows(&[100.0, 100.0, 100.0]), None);
        h.show_rows(&rows(&[100.0, 100.0, 100.0]), Some(&mut ScrollBy(-80.0)));
        // 20 pixels into row 1; row 0 goes.
        assert_eq!(h.show_rows(&[(1, 100.0), (2, 100.0)], None), (20.0, 200.0));
        // If row 1 goes too, the view shows the top of the next one.
        assert_eq!(h.show_rows(&[(2, 100.0), (3, 100.0)], None), (0.0, 200.0));
    }

    #[test]
    fn keeps_the_row_at_the_top_when_reflowing() {
        let mut h = Harness::new();
        h.show_rows(&rows(&[100.0, 100.0, 100.0]), None);
        h.show_rows(&rows(&[100.0, 100.0, 100.0]), Some(&mut ScrollBy(-100.0)));
        // Narrower, the rows get taller; the view stays at the second one.
        h.width = 50.0;
        assert_eq!(
            h.show_rows(&rows(&[150.0, 150.0, 150.0]), None),
            (150.0, 450.0)
        );
    }

    #[test]
    fn moves_by_whole_lines_within_a_reflowed_row() {
        let mut h = Harness::new();
        let row = 10.0 * LINE;
        h.show_rows(&rows(&[row, row]), None);
        // Four lines into the first row; it reflows to half its height.
        h.show_rows(
            &rows(&[row, row]),
            Some(&mut ScrollBy(4.0 * LINE - 2.0 * row + 100.0)),
        );
        h.width = 150.0;
        let (top, _) = h.show_rows(&rows(&[row / 2.0, row / 2.0]), None);
        // iced rounds the offset.
        assert!((top - 2.0 * LINE).abs() < 1.0, "{top}");
    }

    #[test]
    fn a_width_change_doesnt_grow_the_empty_space() {
        let mut h = Harness::new();
        h.show_rows(&rows(&[100.0, 100.0, 100.0]), None);
        // Wider, the rows get shorter. The view would stay at the last row,
        // but that would leave empty space below it; it stays at the
        // bottom instead.
        h.width = 150.0;
        assert_eq!(
            h.show_rows(&rows(&[80.0, 80.0, 80.0]), None),
            (140.0, 240.0)
        );
        // Empty space already there may stay, but not grow.
        assert_eq!(
            h.show_rows(&rows(&[80.0, 80.0, 50.0]), None),
            (140.0, 240.0)
        );
        h.width = 200.0;
        assert_eq!(h.show_rows(&rows(&[60.0, 60.0, 40.0]), None), (90.0, 190.0));
    }
}
