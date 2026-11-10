//! [`Grid`]: the layout of rows and tables, which shares out the width
//! when there isn't enough of it.
//!
//! iced's own `row` and `table` give each child, in order, all the width
//! the ones before it left: a long label leaves nothing for what follows
//! it, and a field that fills the width gets none.

use iced::advanced::layout::{self, Layout, Limits};
use iced::advanced::renderer::{self, Renderer as _};
use iced::advanced::widget::{Operation, Tree};
use iced::advanced::{Clipboard, Shell, mouse, overlay};
use iced::{Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector};

use crate::{Message, spacing};

/// The least width a widget that fills the width (e.g. a text field) gets
/// in a row or a table column, unless the island is narrower.
const MIN_FILL_WIDTH: f32 = 100.0;

/// The padding of a table cell: across and down.
const CELL_PADDING: (f32, f32) = (10.0, 5.0);

/// The width of the lines between table cells.
const SEPARATOR: f32 = 1.0;

/// Children laid out as a row or a table.
pub(crate) struct Grid<'a> {
    children: Vec<Element<'a, Message>>,
    kind: GridKind,
    /// Whether it fills the width: if any child does.
    fill: bool,
}

enum GridKind {
    /// A row, with a gap between visible children. Children that don't fit
    /// go on the next line; hidden ones take no space and get no gap.
    Row { visible: Vec<bool> },
    /// A table, its cells given row by row. Lines are drawn between rows,
    /// and between columns if `column_lines`.
    Table { columns: usize, column_lines: bool },
}

/// Where a table's lines go, kept from layout for drawing.
#[derive(Default)]
struct TableLines {
    /// The x of each line between columns.
    xs: Vec<f32>,
    /// The y of each line between rows.
    ys: Vec<f32>,
}

impl<'a> Grid<'a> {
    /// A row of `children`; `visible` says which are shown.
    pub(crate) fn row(children: Vec<Element<'a, Message>>, visible: Vec<bool>) -> Self {
        let fill = (children.iter().zip(&visible)).any(|(c, v)| *v && fills(c));
        Grid {
            children,
            kind: GridKind::Row { visible },
            fill,
        }
    }

    /// A table with `columns` columns of `cells`, given row by row (so
    /// their number is a multiple of `columns`, which isn't zero).
    pub(crate) fn table(
        cells: Vec<Element<'a, Message>>,
        columns: usize,
        column_lines: bool,
    ) -> Self {
        debug_assert!(columns > 0 && cells.len().is_multiple_of(columns));
        let fill = cells.iter().any(fills);
        Grid {
            children: cells,
            kind: GridKind::Table {
                columns,
                column_lines,
            },
            fill,
        }
    }

    fn layout_row(
        &mut self,
        visible: &[bool],
        tree: &mut Tree,
        renderer: &Renderer,
        max_width: f32,
    ) -> layout::Node {
        let gap = spacing();
        let measure = Limits::new(Size::ZERO, Size::new(max_width, f32::INFINITY));
        // Each line's children, by index, with the width they want: what
        // they take given the whole width, or for those that fill, the
        // least they get.
        let mut lines: Vec<Vec<(usize, f32)>> = vec![Vec::new()];
        let mut x = 0.0;
        let mut nodes: Vec<Option<layout::Node>> = Vec::new();
        for (i, child) in self.children.iter_mut().enumerate() {
            if !visible[i] {
                nodes.push(None);
                continue;
            }
            let (node, want) = if fills(child) {
                (None, MIN_FILL_WIDTH.min(max_width))
            } else {
                let node =
                    (child.as_widget_mut()).layout(&mut tree.children[i], renderer, &measure);
                let width = node.size().width;
                (Some(node), width)
            };
            nodes.push(node);
            let line = lines.last_mut().expect("there's always a line");
            if !line.is_empty() && x + gap + want > max_width {
                lines.push(vec![(i, want)]);
                x = want;
            } else {
                x += if line.is_empty() { want } else { gap + want };
                lines.last_mut().unwrap().push((i, want));
            }
        }

        let mut y = 0.0;
        let mut width: f32 = 0.0;
        for (n, line) in lines.iter().enumerate() {
            if line.is_empty() {
                continue;
            }
            if n > 0 {
                y += gap;
            }
            // Those that fill share what the others leave.
            let fill_count = (line.iter()).filter(|(i, _)| nodes[*i].is_none()).count();
            let taken: f32 = (line.iter())
                .filter(|(i, _)| nodes[*i].is_some())
                .map(|(_, w)| w)
                .sum::<f32>()
                + gap * (line.len() - 1) as f32;
            let share = if max_width.is_finite() && fill_count > 0 {
                ((max_width - taken) / fill_count as f32).max(0.0)
            } else {
                MIN_FILL_WIDTH
            };
            let mut x = 0.0;
            let mut height: f32 = 0.0;
            for (k, &(i, _)) in line.iter().enumerate() {
                if k > 0 {
                    x += gap;
                }
                let node = match nodes[i].take() {
                    Some(node) => node,
                    None => {
                        let limits = Limits::new(Size::ZERO, Size::new(share, f32::INFINITY));
                        (self.children[i].as_widget_mut()).layout(
                            &mut tree.children[i],
                            renderer,
                            &limits,
                        )
                    }
                };
                let size = node.size();
                nodes[i] = Some(node.move_to((x, y)));
                x += size.width;
                height = height.max(size.height);
            }
            width = width.max(x);
            y += height;
        }

        // Hidden children: nothing, wherever.
        let children = (nodes.into_iter().enumerate())
            .map(|(i, node)| {
                node.unwrap_or_else(|| {
                    (self.children[i].as_widget_mut()).layout(
                        &mut tree.children[i],
                        renderer,
                        &Limits::new(Size::ZERO, Size::ZERO),
                    )
                })
            })
            .collect();
        let width = if self.fill && max_width.is_finite() {
            max_width
        } else {
            width
        };
        layout::Node::with_children(Size::new(width, y), children)
    }

    fn layout_table(
        &mut self,
        columns: usize,
        column_lines: bool,
        tree: &mut Tree,
        renderer: &Renderer,
        max_width: f32,
    ) -> layout::Node {
        let (pad_x, pad_y) = CELL_PADDING;
        let column_gap = if column_lines { SEPARATOR } else { 0.0 };
        let rows = self.children.len() / columns;
        let room = max_width - columns as f32 * 2.0 * pad_x - (columns - 1) as f32 * column_gap;
        let room = room.max(0.0);

        // The width each column wants: what its widest cell takes given all
        // the room, and at least MIN_FILL_WIDTH if a cell fills.
        let measure = Limits::new(Size::ZERO, Size::new(room, f32::INFINITY));
        let mut wants = vec![0.0_f32; columns];
        let mut filling = vec![false; columns];
        for (i, cell) in self.children.iter_mut().enumerate() {
            let col = i % columns;
            if fills(cell) {
                filling[col] = true;
                wants[col] = wants[col].max(MIN_FILL_WIDTH);
            } else {
                let node = (cell.as_widget_mut()).layout(&mut tree.children[i], renderer, &measure);
                wants[col] = wants[col].max(node.size().width);
            }
        }
        let (mut widths, left) = share(&wants, room);
        // What's left goes to the columns that fill.
        let fill_count = filling.iter().filter(|f| **f).count();
        if fill_count > 0 && left.is_finite() {
            for (w, _) in widths.iter_mut().zip(&filling).filter(|(_, f)| **f) {
                *w += left / fill_count as f32;
            }
        }

        let mut nodes = Vec::with_capacity(self.children.len());
        let mut heights = vec![0.0_f32; rows];
        let mut used = vec![0.0_f32; columns];
        for (i, cell) in self.children.iter_mut().enumerate() {
            let (row, col) = (i / columns, i % columns);
            let limits = Limits::new(Size::ZERO, Size::new(widths[col], f32::INFINITY));
            let node = (cell.as_widget_mut()).layout(&mut tree.children[i], renderer, &limits);
            heights[row] = heights[row].max(node.size().height);
            used[col] = used[col].max(node.size().width);
            nodes.push(node);
        }
        // A column that doesn't fill is only as wide as its cells came out.
        for col in 0..columns {
            if !filling[col] {
                widths[col] = used[col].min(widths[col]);
            }
        }

        let mut lines = TableLines::default();
        let mut xs = Vec::with_capacity(columns);
        let mut x = 0.0;
        for (col, width) in widths.iter().enumerate() {
            if col > 0 {
                lines.xs.push(x);
                x += column_gap;
            }
            xs.push(x + pad_x);
            x += width + 2.0 * pad_x;
        }
        let mut y = 0.0;
        for (row, height) in heights.iter().enumerate() {
            if row > 0 {
                lines.ys.push(y);
                y += SEPARATOR;
            }
            for col in 0..columns {
                let node = &mut nodes[row * columns + col];
                node.move_to_mut((xs[col], y + pad_y));
            }
            y += height + 2.0 * pad_y;
        }
        *tree.state.downcast_mut::<TableLines>() = lines;
        layout::Node::with_children(Size::new(x, y), nodes)
    }
}

/// Whether `element` fills the width.
fn fills(element: &Element<'_, Message>) -> bool {
    element.as_widget().size().width.is_fill()
}

/// Shares out `total` between columns that want `wants`: each gets what it
/// wants, or else an equal share of what the narrower ones leave. Returns
/// the widths and what's left over.
fn share(wants: &[f32], total: f32) -> (Vec<f32>, f32) {
    let mut order: Vec<usize> = (0..wants.len()).collect();
    order.sort_by(|a, b| wants[*a].total_cmp(&wants[*b]));
    let mut widths = vec![0.0; wants.len()];
    let mut left = total;
    for (k, &i) in order.iter().enumerate() {
        let fair = left / (wants.len() - k) as f32;
        widths[i] = wants[i].min(fair);
        left -= widths[i];
    }
    (widths, left)
}

impl iced::advanced::Widget<Message, Theme, Renderer> for Grid<'_> {
    fn tag(&self) -> iced::advanced::widget::tree::Tag {
        iced::advanced::widget::tree::Tag::of::<TableLines>()
    }

    fn state(&self) -> iced::advanced::widget::tree::State {
        iced::advanced::widget::tree::State::new(TableLines::default())
    }

    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&self.children);
    }

    fn size(&self) -> Size<Length> {
        let width = if self.fill {
            Length::Fill
        } else {
            Length::Shrink
        };
        Size::new(width, Length::Shrink)
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &Limits) -> layout::Node {
        let max_width = limits.max().width;
        let node = match &self.kind {
            GridKind::Row { visible } => {
                let visible = visible.clone();
                self.layout_row(&visible, tree, renderer, max_width)
            }
            &GridKind::Table {
                columns,
                column_lines,
            } => self.layout_table(columns, column_lines, tree, renderer, max_width),
        };
        let size = limits.resolve(self.size().width, Length::Shrink, node.size());
        let children = node.children().to_vec();
        layout::Node::with_children(size, children)
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
            (self.children.iter_mut())
                .zip(&mut tree.children)
                .zip(layout.children())
                .for_each(|((child, state), layout)| {
                    child
                        .as_widget_mut()
                        .operate(state, layout, renderer, operation);
                });
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
        for ((child, state), layout) in (self.children.iter_mut())
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            child.as_widget_mut().update(
                state, event, layout, cursor, renderer, clipboard, shell, viewport,
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
            .map(|((child, state), layout)| {
                (child.as_widget()).mouse_interaction(state, layout, cursor, viewport, renderer)
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
        for ((child, state), layout) in (self.children.iter())
            .zip(&tree.children)
            .zip(layout.children())
            .filter(|(_, layout)| layout.bounds().intersects(viewport))
        {
            (child.as_widget()).draw(state, renderer, theme, style, layout, cursor, viewport);
        }
        let GridKind::Table { column_lines, .. } = self.kind else {
            return;
        };
        // As iced's table draws them.
        let color = theme.extended_palette().background.strong.color;
        let lines = tree.state.downcast_ref::<TableLines>();
        let bounds = layout.bounds();
        let quad = |bounds| renderer::Quad {
            bounds,
            snap: true,
            ..renderer::Quad::default()
        };
        if column_lines {
            for x in &lines.xs {
                let line = Rectangle::new(
                    bounds.position() + Vector::new(*x, 0.0),
                    Size::new(SEPARATOR, bounds.height),
                );
                renderer.fill_quad(quad(line), color);
            }
        }
        for y in &lines.ys {
            let line = Rectangle::new(
                bounds.position() + Vector::new(0.0, *y),
                Size::new(bounds.width, SEPARATOR),
            );
            renderer.fill_quad(quad(line), color);
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

impl<'a> From<Grid<'a>> for Element<'a, Message> {
    fn from(grid: Grid<'a>) -> Self {
        Element::new(grid)
    }
}

#[cfg(test)]
mod tests {
    use super::share;

    #[test]
    fn share_gives_narrow_columns_what_they_want_and_splits_the_rest() {
        assert_eq!(share(&[10.0, 20.0], 100.0), (vec![10.0, 20.0], 70.0));
        assert_eq!(
            share(&[10.0, 200.0, 300.0], 100.0),
            (vec![10.0, 45.0, 45.0], 0.0)
        );
        assert_eq!(share(&[60.0, 60.0], 100.0), (vec![50.0, 50.0], 0.0));
    }
}
