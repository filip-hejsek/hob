//! A column whose children keep their widget state (focus, cursor) by key.
//!
//! iced matches widget state to children by position, so removing a child
//! would give the ones after it the state of their predecessors. iced's
//! `keyed_column` only handles a single run of children added or removed;
//! this one handles any change.

use std::any::Any;
use std::collections::HashMap;
use std::hash::Hash;
use std::mem;

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, mouse, overlay, renderer};
use iced::widget::{Column, column};
use iced::{Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector};

/// A [`Column`] whose children are matched to their state by key.
pub(crate) struct Keyed<'a, K, Message> {
    keys: Vec<K>,
    column: Column<'a, Message>,
}

/// Creates a keyed column. Children with equal keys must be interchangeable
/// (e.g. plain text), since their states may be swapped.
pub(crate) fn keyed<'a, K, Message>(
    children: impl IntoIterator<Item = (K, Element<'a, Message>)>,
) -> Keyed<'a, K, Message> {
    let (keys, children): (Vec<_>, Vec<_>) = children.into_iter().unzip();
    Keyed {
        keys,
        column: column(children),
    }
}

impl<K, Message> Keyed<'_, K, Message> {
    /// Sets the space between children.
    pub(crate) fn spacing(mut self, amount: f32) -> Self {
        self.column = self.column.spacing(amount);
        self
    }
}

/// Marks state taken by no widget, so iced creates a new one.
struct Placeholder;

/// A [`Keyed`]'s state: its keys, and how its children moved in the last
/// diff.
pub(crate) struct KeyedState {
    /// The keys, a `Vec<K>`. Not generic, so others can read
    /// [`moved`](Self::moved) without knowing the key type.
    keys: Box<dyn Any>,
    /// For each child before the last diff, its index after it, or `None`
    /// if it's gone. `None` if the keys didn't change.
    pub(crate) moved: Option<Vec<Option<usize>>>,
}

impl<K, Message> Widget<Message, Theme, Renderer> for Keyed<'_, K, Message>
where
    K: Copy + Eq + Hash + 'static,
{
    fn size(&self) -> Size<Length> {
        self.column.size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.column.size_hint()
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<KeyedState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(KeyedState {
            keys: Box::new(self.keys.clone()),
            moved: None,
        })
    }

    fn children(&self) -> Vec<Tree> {
        self.column.children()
    }

    fn diff(&self, tree: &mut Tree) {
        let state = tree.state.downcast_mut::<KeyedState>();
        state.moved = None;
        // Another key type only if another keyed column took this place.
        let keys = match state.keys.downcast_mut::<Vec<K>>() {
            Some(keys) => mem::take(keys),
            None => Vec::new(),
        };
        if keys != self.keys {
            // Equal keys go in order, so each queue holds the old indices of
            // one key.
            let mut old: HashMap<K, Vec<(usize, Tree)>> = HashMap::new();
            for (i, (key, child)) in keys.iter().zip(mem::take(&mut tree.children)).enumerate() {
                old.entry(*key).or_default().push((i, child));
            }
            for queue in old.values_mut() {
                queue.reverse();
            }
            let mut moved = vec![None; keys.len()];
            tree.children = (self.keys.iter().enumerate())
                .map(|(i, key)| match old.get_mut(key).and_then(Vec::pop) {
                    Some((j, child)) => {
                        moved[j] = Some(i);
                        child
                    }
                    None => Tree {
                        tag: tree::Tag::of::<Placeholder>(),
                        ..Tree::empty()
                    },
                })
                .collect();
            state.moved = Some(moved);
        }
        state.keys = Box::new(self.keys.clone());
        // Now that the children line up, the column diffs them one by one.
        self.column.diff(tree);
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.column.layout(tree, renderer, limits)
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
        (self.column).draw(tree, renderer, theme, style, layout, cursor, viewport);
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.column.operate(tree, layout, renderer, operation);
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
        (self.column).update(
            tree, event, layout, cursor, renderer, clipboard, shell, viewport,
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
        (self.column).mouse_interaction(tree, layout, cursor, viewport, renderer)
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        (self.column).overlay(tree, layout, renderer, viewport, translation)
    }
}

impl<'a, K, Message> From<Keyed<'a, K, Message>> for Element<'a, Message>
where
    K: Copy + Eq + Hash + 'static,
    Message: 'a,
{
    fn from(keyed: Keyed<'a, K, Message>) -> Self {
        Element::new(keyed)
    }
}
