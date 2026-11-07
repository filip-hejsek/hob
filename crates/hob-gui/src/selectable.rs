//! [`SelectableText`]: text that can be selected and copied.

use std::borrow::Cow;

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::text::highlighter::PlainText;
use iced::advanced::widget::operation::Focusable;
use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, mouse};
use iced::widget::text;
use iced::widget::text_editor::{self, Action, Binding, Content, KeyPress, TextEditor};
use iced::{Background, Border, Color, Element, Event, Font, Length, Rectangle, Renderer, Size};
use iced::{Theme, padding};

/// Read-only text that looks like iced's `text`, but can be selected with
/// the mouse (or Shift and the arrows) and copied with Ctrl+C. It's a
/// `text_editor` that ignores edits, keeping its [`Content`] in its widget
/// state like kettle-ui-iced's `TextArea`.
///
/// It focuses itself only while it has a selection, so a click on it shows
/// no cursor, and focus elsewhere (e.g. a new island) clears the selection.
/// It has no Id, so the shell's focus rules see it as nothing having focus.
pub(crate) struct SelectableText<'a> {
    value: Cow<'a, str>,
    font: Option<Font>,
    style: fn(&Theme) -> text::Style,
}

/// The widget state: the editor's content, and the text it holds.
struct State {
    text: String,
    content: Content,
}

type Editor<'b> = TextEditor<'b, PlainText, Action, Theme, Renderer>;

/// Shows `value` as selectable text.
pub(crate) fn selectable<'a>(value: impl Into<Cow<'a, str>>) -> SelectableText<'a> {
    SelectableText {
        value: value.into(),
        font: None,
        style: text::default,
    }
}

impl SelectableText<'_> {
    pub(crate) fn font(mut self, font: Font) -> Self {
        self.font = Some(font);
        self
    }

    /// Its colour, from a style of iced's `text`.
    pub(crate) fn style(mut self, style: fn(&Theme) -> text::Style) -> Self {
        self.style = style;
        self
    }

    /// The iced editor for `content`, looking like plain text.
    fn editor<'b>(&'b self, content: &'b Content) -> Editor<'b> {
        let style = self.style;
        let mut editor = TextEditor::new(content)
            .padding(padding::all(0))
            .key_binding(key_binding)
            .on_action(|action| action)
            .style(move |theme: &Theme, _| {
                let value = style(theme).color.unwrap_or(theme.palette().text);
                text_editor::Style {
                    background: Background::Color(Color::TRANSPARENT),
                    border: Border::default(),
                    placeholder: value,
                    value,
                    selection: theme.extended_palette().primary.weak.color,
                }
            });
        if let Some(font) = self.font {
            editor = editor.font(font);
        }
        editor
    }
}

/// The editor's usual keys, without those that edit or only move the cursor.
fn key_binding(key: KeyPress) -> Option<Binding<Action>> {
    match Binding::from_key_press(key)? {
        binding @ (Binding::Copy
        | Binding::Select(_)
        | Binding::SelectWord
        | Binding::SelectLine
        | Binding::SelectAll
        | Binding::Unfocus) => Some(binding),
        _ => None,
    }
}

impl<M> Widget<M, Theme, Renderer> for SelectableText<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State {
            text: self.value.to_string(),
            content: Content::with_text(&self.value),
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
            state.text = self.value.to_string();
            state.content = Content::with_text(&self.value);
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
        self.editor(&state.content)
            .layout(&mut tree.children[0], renderer, limits)
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
        self.editor(&state.content).draw(
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
        // So that tests (iced_test) can find the text.
        operation.text(None, layout.bounds(), &state.text);
        self.editor(&state.content)
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        // The wheel scrolls the scrollback, not the editor.
        if let Event::Mouse(mouse::Event::WheelScrolled { .. }) = event {
            return;
        }
        let state = tree.state.downcast_mut::<State>();
        let mut actions = Vec::new();
        let mut inner = Shell::new(&mut actions);
        self.editor(&state.content).update(
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
        if actions.is_empty() {
            return;
        }
        for action in actions.into_iter().filter(|a| !a.is_edit()) {
            state.content.perform(action);
        }
        let editor = (tree.children[0].state).downcast_mut::<text_editor::State<PlainText>>();
        match (state.content.selection().is_some(), editor.is_focused()) {
            (true, false) => editor.focus(),
            (false, true) => editor.unfocus(),
            _ => {}
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
        self.editor(&state.content).mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }
}

impl<'a, M: 'a> From<SelectableText<'a>> for Element<'a, M> {
    fn from(text: SelectableText<'a>) -> Self {
        Element::new(text)
    }
}

/// Passes on what the editor asked of its shell, except its messages and
/// input method (it takes no input).
fn forward<M>(inner: &Shell<'_, Action>, shell: &mut Shell<'_, M>) {
    if inner.is_event_captured() {
        shell.capture_event();
    }
    shell.request_redraw_at(inner.redraw_request());
    if inner.is_layout_invalid() {
        shell.invalidate_layout();
    }
    if inner.are_widgets_invalid() {
        shell.invalidate_widgets();
    }
}
