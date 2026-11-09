//! TODO items: things that can be done now. They aren't stored; each source
//! makes its items from the notes whenever `todo` asks.

use crate::db::{Db, Note};
use crate::describe;

/// Something to do now.
#[derive(Debug)]
pub(crate) enum Item {
    /// Work on an open TODO note.
    Do(Note),
    /// Give an unnamed note a name. Unnamed TODO notes aren't offered: their
    /// text says what they are.
    Name(Note),
}

impl Item {
    /// The item's button label.
    pub(crate) fn label(&self) -> String {
        match self {
            Item::Do(note) => format!("Do {}", describe(note)),
            Item::Name(note) => format!("Name {}", describe(note)),
        }
    }
}

/// Up to `n` items, taken from the sources in turn.
pub(crate) fn pick(db: &Db, n: usize) -> rusqlite::Result<Vec<Item>> {
    let sources = vec![
        db.open_todos()?.into_iter().map(Item::Do).collect(),
        db.unnamed()?.into_iter().map(Item::Name).collect(),
    ];
    Ok(interleave(sources, n))
}

/// Up to `n` items, taken from the sources in turn, so that many items of
/// one kind don't hide the others. Each source gives its oldest first.
fn interleave<T>(sources: Vec<Vec<T>>, n: usize) -> Vec<T> {
    let mut sources: Vec<_> = sources.into_iter().map(Vec::into_iter).collect();
    let mut items = Vec::new();
    while items.len() < n {
        let before = items.len();
        for source in &mut sources {
            if items.len() < n
                && let Some(item) = source.next()
            {
                items.push(item);
            }
        }
        if items.len() == before {
            break;
        }
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_take_turns() {
        let sources = vec![vec![1, 2, 3, 4], vec![10], vec![]];
        assert_eq!(interleave(sources.clone(), 4), [1, 10, 2, 3]);
        assert_eq!(interleave(sources.clone(), 1), [1]);
        assert_eq!(interleave(sources, 9), [1, 10, 2, 3, 4]);
        assert_eq!(interleave(Vec::<Vec<i32>>::new(), 3), [0; 0]);
    }

    #[test]
    fn an_unnamed_todo_note_is_offered_once() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("notes.sqlite")).unwrap();
        db.add(None, "todo call Ada").unwrap();
        db.add(None, "todos are fun").unwrap();
        let labels: Vec<_> = pick(&db, 3).unwrap().iter().map(Item::label).collect();
        assert_eq!(labels, ["Do #1: todo call Ada", "Name #2: todos are fun"]);
    }
}
