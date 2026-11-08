//! TODO items: things that can be done now. They aren't stored; each source
//! makes its items from the notes whenever `todo` asks.

use crate::db::{Db, Note};
use crate::describe;

/// Something to do now.
#[derive(Debug)]
pub(crate) enum Item {
    /// Work on a TODO note: one whose title or text starts with "todo".
    Do(Note),
    /// Give an unnamed note a name.
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

/// Up to `n` items, taken from the sources in turn, so that many items of
/// one kind don't hide the others. Each source gives its oldest first.
pub(crate) fn pick(db: &Db, n: usize) -> rusqlite::Result<Vec<Item>> {
    let sources = [
        db.open_todos(n)?
            .into_iter()
            .map(Item::Do)
            .collect::<Vec<_>>(),
        db.unnamed(n)?.into_iter().map(Item::Name).collect(),
    ];
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
    Ok(items)
}
