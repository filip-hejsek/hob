//! [`TextArea`]: a multi-line text input that takes its value as a string.

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::text::Renderer as _;
use iced::advanced::text::highlighter::PlainText;
use iced::advanced::widget::{Id, Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, mouse};
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::text_editor::{self, Action, Content, Status, TextEditor};
use iced::{Element, Event, Length, Rectangle, Renderer, Size, Theme, window};

use crate::Message;

/// The height of an empty text area, in lines. It grows with its text.
const MIN_LINES: f32 = 4.0;

/// A multi-line text input. Like iced's `text_input` (and unlike its
/// `text_editor`), it takes its value as a string: it keeps the editor's
/// [`Content`] in its widget state, and remakes it only when the value
/// changes from outside, so the cursor survives the host's rebuilds.
pub(crate) struct TextArea<'a> {
    value: &'a str,
    placeholder: &'a str,
    id: Option<Id>,
    on_change: Option<Box<dyn Fn(String) -> Message + 'a>>,
}

/// The widget state: the editor's content, and the text it holds.
struct State {
    text: String,
    content: Content,
    /// The status at the last redraw, which decides the style.
    status: Status,
}

type Editor<'b> = TextEditor<'b, PlainText, Action, Theme, Renderer>;

impl<'a> TextArea<'a> {
    pub(crate) fn new(value: &'a str, placeholder: &'a str) -> Self {
        TextArea {
            value,
            placeholder,
            id: None,
            on_change: None,
        }
    }

    pub(crate) fn id(mut self, id: Id) -> Self {
        self.id = Some(id);
        self
    }

    /// Without it, the text area is disabled.
    pub(crate) fn on_change_maybe(mut self, f: Option<impl Fn(String) -> Message + 'a>) -> Self {
        self.on_change = f.map(|f| Box::new(f) as _);
        self
    }

    /// The iced editor for `content`, styled for `status`. It's made anew
    /// for every call, so it can't keep its own status.
    fn editor<'b>(
        &'b self,
        content: &'b Content,
        status: Status,
        renderer: &Renderer,
    ) -> Editor<'b> {
        let size = renderer.default_size();
        let min_height = MIN_LINES * LineHeight::default().to_absolute(size).0;
        let mut editor = TextEditor::new(content)
            .placeholder(self.placeholder)
            .min_height(min_height)
            .wrapping(Wrapping::WordOrGlyph)
            .style(move |theme, _| text_editor::default(theme, status));
        if let Some(id) = &self.id {
            editor = editor.id(id.clone());
        }
        if self.on_change.is_some() {
            editor = editor.on_action(|action| action);
        }
        editor
    }
}

impl Widget<Message, Theme, Renderer> for TextArea<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State {
            text: self.value.to_owned(),
            content: Content::with_text(self.value),
            status: if self.on_change.is_some() {
                Status::Active
            } else {
                Status::Disabled
            },
        })
    }

    fn children(&self) -> Vec<Tree> {
        // The editor's state doesn't depend on its content.
        let content = Content::new();
        let editor: Editor<'_> = TextEditor::new(&content);
        vec![Tree::new(&editor as &dyn Widget<Action, Theme, Renderer>)]
    }

    fn diff(&self, tree: &mut Tree) {
        let state = tree.state.downcast_mut::<State>();
        if state.text != self.value {
            state.text = self.value.to_owned();
            state.content = Content::with_text(self.value);
        }
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let state = tree.state.downcast_ref::<State>();
        self.editor(&state.content, state.status, renderer).layout(
            &mut tree.children[0],
            renderer,
            limits,
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
        let state = tree.state.downcast_ref::<State>();
        self.editor(&state.content, state.status, renderer).draw(
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
        let state = tree.state.downcast_ref::<State>();
        self.editor(&state.content, state.status, renderer).operate(
            &mut tree.children[0],
            layout,
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
        let state = tree.state.downcast_mut::<State>();
        let mut actions = Vec::new();
        let mut inner = Shell::new(&mut actions);
        self.editor(&state.content, state.status, renderer).update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            &mut inner,
            viewport,
        );
        forward(&inner, shell);

        // As in iced's text editor, which can't keep it here.
        let focused = (tree.children[0].state)
            .downcast_ref::<text_editor::State<PlainText>>()
            .is_focused();
        let is_hovered = cursor.is_over(layout.bounds());
        let status = if self.on_change.is_none() {
            Status::Disabled
        } else if focused {
            Status::Focused { is_hovered }
        } else if is_hovered {
            Status::Hovered
        } else {
            Status::Active
        };
        if let Event::Window(window::Event::RedrawRequested(_)) = event {
            state.status = status;
        } else if status != state.status {
            shell.request_redraw();
        }

        if actions.is_empty() {
            return;
        }
        let edited = actions.iter().any(Action::is_edit);
        for action in actions {
            state.content.perform(action);
        }
        if edited {
            state.text = state.content.text();
            if let Some(on_change) = &self.on_change {
                shell.publish(on_change(state.text.clone()));
            }
        }
        shell.request_redraw();
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let state = tree.state.downcast_ref::<State>();
        self.editor(&state.content, state.status, renderer)
            .mouse_interaction(&tree.children[0], layout, cursor, viewport, renderer)
    }
}

impl<'a> From<TextArea<'a>> for Element<'a, Message> {
    fn from(area: TextArea<'a>) -> Self {
        Element::new(area)
    }
}

/// Passes on what the editor asked of its shell, except its messages.
fn forward(inner: &Shell<'_, Action>, shell: &mut Shell<'_, Message>) {
    if inner.is_event_captured() {
        shell.capture_event();
    }
    shell.request_redraw_at(inner.redraw_request());
    shell.request_input_method(inner.input_method());
    if inner.is_layout_invalid() {
        shell.invalidate_layout();
    }
    if inner.are_widgets_invalid() {
        shell.invalidate_widgets();
    }
}
