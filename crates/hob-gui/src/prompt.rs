//! The command prompt: a text editor with a bold `>` before it, like the
//! command lines in the scrollback.
//!
//! It's a `text_editor` rather than a `text_input`, since the shell can read
//! an editor's cursor (from its `Content`) and an editor can take key
//! bindings. The `>` is coloured while the editor has focus, and a click on
//! it focuses the editor.
//!
//! The `>` and the editor are in a scrollable, the editor at its full
//! height, so a scrollbar shows when not all of it fits, and the `>` scrolls
//! with the first row. The editor doesn't scroll itself then, so the prompt
//! keeps the cursor in view: it lays the text out again (as the editor
//! does) to find the cursor's row, and scrolls to it when the text or cursor
//! changed, leaving half a row above and below it.

use iced::advanced::layout::{self, Layout};
use iced::advanced::text::highlighter::PlainText;
use iced::advanced::text::{self as advanced_text, LineHeight, Paragraph as _, Renderer as _};
use iced::advanced::widget::operation::{Focusable, Scrollable};
use iced::advanced::widget::{Id, Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, mouse, renderer};
use iced::alignment;
use iced::widget::scrollable::{AbsoluteOffset, Direction, Scrollbar};
use iced::widget::text::{Shaping, Wrapping};
use iced::widget::text_editor::{Content, Position};
use iced::widget::{Scrollable as ScrollableWidget, container, row, scrollable, text, text_editor};
use iced::{Element, Event, Font, Length, Rectangle, Renderer, Size, Theme, Vector, font, padding};
use iced_graphics::text::Paragraph;

use crate::selectable::boxless;

/// The editor's padding above and below its text, for the `>` to line up
/// with it.
pub(crate) const TOP: f32 = 5.0;

/// How many rows the prompt shows before it scrolls.
const MAX_ROWS: f32 = 10.0;

/// How long lines wrap, in the editor and when finding the cursor's row.
const WRAPPING: Wrapping = Wrapping::WordOrGlyph;

/// The font of command lines, and of the prompt's `>`.
pub(crate) const BOLD: Font = Font {
    weight: font::Weight::Bold,
    ..Font::DEFAULT
};

/// The prompt; see the module docs.
pub(crate) struct Prompt<'a, Message> {
    /// The `>` and the editor, in a scrollable.
    scrollable: ScrollableWidget<'a, Message>,
    /// The editor's text and placeholder: tests (iced_test) find the prompt
    /// by its text, or its placeholder if empty.
    content: &'a Content,
    placeholder: &'a str,
}

/// The editor of a [`Prompt`].
pub(crate) type Editor<'a, Message> = text_editor::TextEditor<'a, PlainText, Message>;

/// A prompt editing `content`, with a `placeholder` shown while it's
/// empty. The editor needs only its id, messages and key bindings set.
pub(crate) fn prompt<'a, Message: 'a>(
    content: &'a Content,
    placeholder: &'a str,
    editor: impl FnOnce(Editor<'a, Message>) -> Editor<'a, Message>,
) -> Prompt<'a, Message> {
    let editor = editor(text_editor::TextEditor::new(content))
        .placeholder(placeholder)
        .padding(padding::vertical(TOP))
        .wrapping(WRAPPING)
        .style(|theme, status| boxless(text_editor::default(theme, status)));
    let icon = container(text(">").font(BOLD)).padding(padding::top(TOP));
    let scrollable = scrollable(row![icon, editor].spacing(4.0))
        .width(Length::Fill)
        .direction(Direction::Vertical(Scrollbar::new().spacing(4.0)));
    Prompt {
        scrollable,
        content,
        placeholder,
    }
}

/// What the prompt last laid out, to tell whether to scroll to the cursor.
#[derive(Default)]
struct State {
    /// The text, laid out at `width`, as the editor lays it out.
    text: String,
    width: f32,
    rows: Paragraph,
    cursor: Option<Position>,
    /// The height it was laid out to.
    height: f32,
}

impl<Message> Prompt<'_, Message> {
    /// The scrollable, as a widget.
    fn inner(&self) -> &dyn Widget<Message, Theme, Renderer> {
        &self.scrollable
    }
}

/// The editor's state, in the prompt's tree.
fn editor(tree: &mut Tree) -> &mut text_editor::State<PlainText> {
    // The prompt has the scrollable, which has the row of the `>` and the
    // editor.
    tree.children[0].children[0].children[1]
        .state
        .downcast_mut::<text_editor::State<PlainText>>()
}

impl<Message> Widget<Message, Theme, Renderer> for Prompt<'_, Message> {
    fn size(&self) -> Size<Length> {
        self.scrollable.size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.scrollable.size_hint()
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(self.inner())]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&[self.inner()]);
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        // The max height is worked out here, where the text size is known.
        let size = renderer.default_size();
        let row_height = LineHeight::default().to_absolute(size).0;
        let limits = limits.max_height(MAX_ROWS * row_height + 2.0 * TOP);
        let node = (self.scrollable).layout(&mut tree.children[0], renderer, &limits);

        // The editor's width, as the scrollable leaves it.
        let width = node.children()[0].children()[1].size().width;
        let state = tree.state.downcast_mut::<State>();
        let text = self.content.text();
        if text != state.text || width != state.width {
            state.text = text;
            state.width = width;
            state.rows = Paragraph::with_text(advanced_text::Text {
                content: &state.text,
                bounds: Size::new(width, f32::INFINITY),
                size,
                line_height: LineHeight::default(),
                font: renderer.default_font(),
                align_x: advanced_text::Alignment::Left,
                align_y: alignment::Vertical::Top,
                shaping: Shaping::Advanced,
                wrapping: WRAPPING,
            });
        } else if state.cursor == Some(self.content.cursor().position)
            && state.height == node.size().height
        {
            // Nothing moved; the wheel may have scrolled the prompt.
            return node;
        }
        state.height = node.size().height;
        let cursor = self.content.cursor().position;
        state.cursor = Some(cursor);
        if let Some((top, height)) = cursor_row(&state.rows, cursor) {
            // The row is below the editor's padding; leave half a row
            // above and below it.
            let mut reveal = Reveal {
                top: TOP + top - height / 2.0,
                bottom: TOP + top + height * 1.5,
            };
            let layout = Layout::new(&node);
            (self.scrollable).operate(&mut tree.children[0], layout, renderer, &mut reveal);
        }
        node
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let focused = tree.children[0].children[0].children[1]
            .state
            .downcast_ref::<text_editor::State<PlainText>>()
            .is_focused();
        let palette = theme.extended_palette();
        // The `>` takes the inherited colour; the editor has its own style.
        let style = renderer::Style {
            text_color: match focused {
                true => palette.primary.base.color,
                false => palette.background.strong.color,
            },
        };
        let inner = &tree.children[0];
        (self.scrollable).draw(inner, renderer, theme, &style, layout, cursor, viewport);
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        // `text_editor` doesn't report its text.
        let shown = match self.content.is_empty() {
            true => self.placeholder.to_owned(),
            false => self.content.text(),
        };
        operation.text(None, layout.bounds(), &shown);
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
        if shell.is_event_captured() {
            return;
        }
        // A click on the `>` (or next to it) focuses the editor: left of
        // the editor (the scrollable moves things only up and down).
        let row = layout.children().next();
        let editor_x = row
            .and_then(|row| row.children().nth(1))
            .map(|e| e.bounds().x);
        if let Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) = event
            && let Some(position) = cursor.position_over(layout.bounds())
            && editor_x.is_some_and(|x| position.x < x)
        {
            editor(tree).focus();
            shell.request_redraw();
            shell.capture_event();
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
        let inner = &tree.children[0];
        (self.scrollable).mouse_interaction(inner, layout, cursor, viewport, renderer)
    }
}

/// The top and height of the row with `cursor`, in `rows`.
fn cursor_row(rows: &Paragraph, cursor: Position) -> Option<(f32, f32)> {
    let runs = rows.buffer().layout_runs();
    // The last row of the cursor's line that starts at or before it.
    let run = runs
        .filter(|run| run.line_i == cursor.line)
        .take_while(|run| run.glyphs.first().is_none_or(|g| g.start <= cursor.column))
        .last()?;
    Some((run.line_top, run.line_height))
}

/// Scrolls the scrollable so that `top..bottom` of its content is shown,
/// or as much of it as there is.
pub(crate) struct Reveal {
    pub(crate) top: f32,
    pub(crate) bottom: f32,
}

impl Reveal {
    /// The offset to scroll to from `offset`, with `height` of `content`
    /// shown, if it has to change: as little as needed.
    fn offset(&self, offset: f32, height: f32, content: f32) -> Option<f32> {
        let top = self.top.max(0.0);
        let bottom = self.bottom.min(content);
        if top < offset {
            Some(top)
        } else if bottom > offset + height {
            Some(bottom - height)
        } else {
            None
        }
    }
}

impl Operation for Reveal {
    fn scrollable(
        &mut self,
        _: Option<&Id>,
        bounds: Rectangle,
        content: Rectangle,
        translation: Vector,
        state: &mut dyn Scrollable,
    ) {
        if let Some(y) = self.offset(translation.y, bounds.height, content.height) {
            state.scroll_to(AbsoluteOffset {
                x: None,
                y: Some(y),
            });
        }
    }

    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }
}

impl<'a, Message: 'a> From<Prompt<'a, Message>> for Element<'a, Message> {
    fn from(prompt: Prompt<'a, Message>) -> Self {
        Element::new(prompt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `text` laid out like the prompt's, `width` wide, with 10 px text
    /// (rows 13 px high).
    fn rows(text: &str, width: f32) -> Paragraph {
        Paragraph::with_text(advanced_text::Text {
            content: text,
            bounds: Size::new(width, f32::INFINITY),
            size: 10.into(),
            line_height: LineHeight::default(),
            font: Font::DEFAULT,
            align_x: advanced_text::Alignment::Left,
            align_y: alignment::Vertical::Top,
            shaping: Shaping::Advanced,
            wrapping: WRAPPING,
        })
    }

    fn row_of(rows: &Paragraph, line: usize, column: usize) -> usize {
        let (top, height) = cursor_row(rows, Position { line, column }).unwrap();
        (top / height).round() as usize
    }

    #[test]
    fn cursor_row_counts_lines_and_wrapped_rows() {
        let lines = rows("one\ntwo\n\nfour", 1000.0);
        assert_eq!(row_of(&lines, 0, 0), 0);
        assert_eq!(row_of(&lines, 1, 2), 1);
        // An empty line has a row too.
        assert_eq!(row_of(&lines, 2, 0), 2);
        assert_eq!(row_of(&lines, 3, 4), 3);

        let words = "word ".repeat(40);
        let wrapped = rows(&words, 100.0);
        let last = wrapped.buffer().layout_runs().count() - 1;
        assert!(last > 3);
        assert_eq!(row_of(&wrapped, 0, 0), 0);
        assert_eq!(row_of(&wrapped, 0, words.len()), last);
    }

    #[test]
    fn reveal_scrolls_as_little_as_needed() {
        let reveal = Reveal {
            top: 100.0,
            bottom: 120.0,
        };
        // Shown already.
        assert_eq!(reveal.offset(50.0, 100.0, 500.0), None);
        // Above: its top at the top.
        assert_eq!(reveal.offset(110.0, 100.0, 500.0), Some(100.0));
        // Below: its bottom at the bottom.
        assert_eq!(reveal.offset(0.0, 100.0, 500.0), Some(20.0));
        // Past the ends: only as far as the content goes.
        let first = Reveal {
            top: -10.0,
            bottom: 30.0,
        };
        assert_eq!(first.offset(50.0, 100.0, 500.0), Some(0.0));
        let last = Reveal {
            top: 480.0,
            bottom: 520.0,
        };
        assert_eq!(last.offset(0.0, 100.0, 500.0), Some(400.0));
    }
}
