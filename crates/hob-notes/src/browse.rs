//! `notes browse`: shows the notes one at a time, with action buttons that
//! move to the previous or next one.

use std::sync::{Arc, Mutex};

use hob_core::{CommandResult, Context};
use kettle_ui::widget::*;
use kettle_ui::{IslandBuilder, SetState, Value};

use crate::db::{Db, Note};
use crate::{Filter, details, existing, heading};

/// Which way a button moves.
#[derive(Clone, Copy)]
enum Step {
    Previous,
    Next,
}

/// Shows the notes `filter` lets through, one at a time, starting with note
/// `start` or the newest. The buttons read the notes again, so they show
/// changes made meanwhile.
pub(crate) fn browse(
    ctx: &mut Context,
    db: Db,
    filter: Filter,
    start: Option<i64>,
) -> CommandResult {
    let notes = filter.notes(&db)?;
    let index = match start {
        Some(id) => match notes.iter().position(|note| note.id == id) {
            Some(index) => index,
            None => {
                existing(&db, id)?;
                return Err(format!("note #{id} isn't among the notes browsed").into());
            }
        },
        None if notes.is_empty() => {
            ctx.print(filter.none_message(&db)?);
            return Ok(());
        }
        None => notes.len() - 1,
    };

    let mut island = IslandBuilder::new(column([
        label(state("position")),
        label(state("heading")),
        label(state("details")),
        when("has_categories", label(state("categories"))),
        label(state("text")),
        row([
            action_button("previous", "Previous [p]"),
            action_button("next", "Next [n]"),
            cancel_button("Close"),
        ]),
    ]));
    show(&mut island, &notes, index);

    let db = Arc::new(Mutex::new(db));
    let filter = Arc::new(filter);
    for (id, step) in [("previous", Step::Previous), ("next", Step::Next)] {
        let (db, filter) = (db.clone(), filter.clone());
        island.action_with_state(id, &["id"], move |inputs, changes| {
            let Some(&Value::Number(current)) = inputs.state("id") else {
                unreachable!("id is a number");
            };
            match filter.notes(&db.lock().unwrap()) {
                Ok(notes) => match neighbor(&notes, current as i64, step) {
                    Some(index) => show(changes, &notes, index),
                    None => show_message(changes, "No notes left."),
                },
                Err(error) => show_message(changes, &format!("error: {error}")),
            }
        });
    }
    // Closing is all there is to do, so it says nothing.
    ctx.show(island)?;
    Ok(())
}

/// The index of the note after (or before) note `from`, which may be gone
/// by now; past the end it wraps around. `None` if there are no notes.
fn neighbor(notes: &[Note], from: i64, step: Step) -> Option<usize> {
    if notes.is_empty() {
        return None;
    }
    Some(match step {
        Step::Next => (notes.iter().position(|note| note.id > from)).unwrap_or(0),
        Step::Previous => {
            (notes.iter().rposition(|note| note.id < from)).unwrap_or(notes.len() - 1)
        }
    })
}

/// Sets the named state to show `notes[index]`.
fn show(state: &mut impl SetState, notes: &[Note], index: usize) {
    let note = &notes[index];
    state.set_state("id", note.id as f64);
    state.set_state("position", format!("Note {} of {}", index + 1, notes.len()));
    state.set_state("heading", heading(note));
    state.set_state("details", details(note));
    state.set_state("has_categories", !note.categories.is_empty());
    state.set_state(
        "categories",
        format!("Categories: {}", note.categories.join(", ")),
    );
    state.set_state("text", note.text.clone());
}

/// Sets the named state to show `message` instead of a note, keeping the
/// id, so the buttons move on from there.
fn show_message(state: &mut impl SetState, message: &str) {
    state.set_state("position", message);
    for name in ["heading", "details", "categories", "text"] {
        state.set_state(name, "");
    }
    state.set_state("has_categories", false);
}
