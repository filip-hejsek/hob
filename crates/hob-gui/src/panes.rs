//! A column of panes that share its height by priority, for when not all of
//! them fit.
//!
//! Each pane has a minimum, in pixels and lines of the default text. The
//! minimums are given first, in order of priority (each only as far as the
//! pane needs it), and what's left is shared equally among the panes that
//! need more. A pane that fills (its height is [`Length::Fill`]) needs all it
//! can get; the others need their natural height, found by laying them out
//! with the column's whole height. What a pane doesn't take of what it's
//! given goes to those that fill.

use iced::advanced::layout::{self, Layout};
use iced::advanced::text::Renderer as _;
use iced::advanced::widget::{Operation, Tree, Widget};
use iced::advanced::{Clipboard, Shell, mouse, overlay, renderer};
use iced::widget::text::LineHeight;
use iced::{Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector};

/// A pane's minimum height.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Min {
    /// Pixels.
    pub(crate) pixels: f32,
    /// Lines of the default text.
    pub(crate) lines: f32,
}

/// A column of panes; see the module docs.
pub(crate) struct Panes<'a, Message> {
    children: Vec<Element<'a, Message>>,
    mins: Vec<Min>,
    /// The order of priority, by index into `children`.
    priority: Vec<usize>,
}

/// Creates [`Panes`] from top to bottom, with their minimums; `priority`
/// lists their indices, first the one whose minimum comes first.
pub(crate) fn panes<'a, Message>(
    panes: impl IntoIterator<Item = (Element<'a, Message>, Min)>,
    priority: impl IntoIterator<Item = usize>,
) -> Panes<'a, Message> {
    let (children, mins) = panes.into_iter().unzip();
    Panes {
        children,
        mins,
        priority: priority.into_iter().collect(),
    }
}

/// The heights given to panes that need `needs` (infinite for all they can
/// get) with minimums `mins`, from `height`, the minimums in the order of
/// `priority`.
fn share(height: f32, needs: &[f32], mins: &[f32], priority: &[usize]) -> Vec<f32> {
    let mut given = vec![0.0; needs.len()];
    let mut left = height;
    for &i in priority {
        given[i] = mins[i].min(needs[i]).min(left).max(0.0);
        left -= given[i];
    }
    // Equal shares, least needy first, so what one doesn't need goes to
    // the others.
    let mut needy: Vec<usize> = (0..needs.len()).filter(|&i| needs[i] > given[i]).collect();
    needy.sort_by(|&a, &b| (needs[a] - given[a]).total_cmp(&(needs[b] - given[b])));
    for (n, &i) in needy.iter().enumerate() {
        let part = (left / (needy.len() - n) as f32).max(0.0);
        let more = (needs[i] - given[i]).min(part);
        given[i] += more;
        left -= more;
    }
    given
}

impl<Message> Widget<Message, Theme, Renderer> for Panes<'_, Message> {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&self.children);
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let Size { width, height } = limits.max();
        let line = LineHeight::default().to_absolute(renderer.default_size()).0;
        let within = |height| layout::Limits::new(Size::ZERO, Size::new(width, height));

        // Panes that fill aren't laid out until their height is known.
        let mut nodes: Vec<Option<layout::Node>> = (self.children.iter_mut())
            .zip(&mut tree.children)
            .map(|(child, tree)| {
                let fills = child.as_widget().size().height.is_fill();
                (!fills).then(|| {
                    child
                        .as_widget_mut()
                        .layout(tree, renderer, &within(height))
                })
            })
            .collect();
        let needs: Vec<f32> = (nodes.iter())
            .map(|node| node.as_ref().map_or(f32::INFINITY, |n| n.size().height))
            .collect();
        let mins: Vec<f32> = (self.mins.iter())
            .map(|min| min.pixels + min.lines * line)
            .collect();
        let given = share(height, &needs, &mins, &self.priority);

        // The panes that don't fill first: what they don't take of what
        // they're given goes to those that do.
        let mut left = 0.0;
        let mut fills = 0;
        for (((child, tree), node), &given) in (self.children.iter_mut())
            .zip(&mut tree.children)
            .zip(&mut nodes)
            .zip(&given)
        {
            match node {
                // Laid out again if it gets less than it needs.
                Some(n) if n.size().height > given => {
                    *n = child.as_widget_mut().layout(tree, renderer, &within(given));
                    left += given - n.size().height;
                }
                Some(n) => left += given - n.size().height,
                None => fills += 1,
            }
        }
        let mut y = 0.0;
        let children = (self.children.iter_mut())
            .zip(&mut tree.children)
            .zip(nodes.into_iter().zip(given))
            .map(|((child, tree), (node, given))| {
                let node = node.unwrap_or_else(|| {
                    let height = given + left.max(0.0) / fills as f32;
                    child
                        .as_widget_mut()
                        .layout(tree, renderer, &within(height))
                });
                let node = node.move_to((0.0, y));
                y += node.size().height;
                node
            })
            .collect();
        layout::Node::with_children(Size::new(width, height), children)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.container(None, layout.bounds());
        operation.traverse(&mut |operation| {
            for ((child, tree), layout) in (self.children.iter_mut())
                .zip(&mut tree.children)
                .zip(layout.children())
            {
                child
                    .as_widget_mut()
                    .operate(tree, layout, renderer, operation);
            }
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
        for ((child, tree), layout) in (self.children.iter_mut())
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            child.as_widget_mut().update(
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
        (self.children.iter())
            .zip(&tree.children)
            .zip(layout.children())
            .map(|((child, tree), layout)| {
                (child.as_widget()).mouse_interaction(tree, layout, cursor, viewport, renderer)
            })
            .max()
            .unwrap_or_default()
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
        for ((child, tree), layout) in (self.children.iter())
            .zip(&tree.children)
            .zip(layout.children())
        {
            // A pane that got less than its natural height draws only
            // within what it got.
            if let Some(clip) = layout.bounds().intersection(viewport) {
                (child.as_widget()).draw(tree, renderer, theme, style, layout, cursor, &clip);
            }
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        overlay::from_children(
            &mut self.children,
            tree,
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message: 'a> From<Panes<'a, Message>> for Element<'a, Message> {
    fn from(panes: Panes<'a, Message>) -> Self {
        Element::new(panes)
    }
}

#[cfg(test)]
mod tests {
    use super::share;

    const ALL: f32 = f32::INFINITY;

    #[test]
    fn everything_fits() {
        // Scrollback, offer, prompt.
        let given = share(500.0, &[ALL, 100.0, 30.0], &[50.0, 20.0, 30.0], &[2, 1, 0]);
        assert_eq!(given, [370.0, 100.0, 30.0]);
    }

    #[test]
    fn minimums_come_first_by_priority() {
        let needs = [ALL, 400.0, 30.0];
        let mins = [50.0, 20.0, 30.0];
        assert_eq!(share(40.0, &needs, &mins, &[2, 1, 0]), [0.0, 10.0, 30.0]);
        assert_eq!(share(80.0, &needs, &mins, &[2, 1, 0]), [30.0, 20.0, 30.0]);
    }

    #[test]
    fn the_rest_is_shared_equally() {
        let mins = [50.0, 20.0, 30.0];
        // 200 left: 100 each.
        let given = share(300.0, &[ALL, 400.0, 30.0], &mins, &[2, 1, 0]);
        assert_eq!(given, [150.0, 120.0, 30.0]);
        // What one doesn't need goes to the others.
        let given = share(400.0, &[ALL, 400.0, 70.0], &mins, &[2, 1, 0]);
        assert_eq!(given, [180.0, 150.0, 70.0]);
    }
}
