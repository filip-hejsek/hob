//! `notes search`: a search box, the best few matches, and a preview of the
//! top one, updated as the user types.

use std::cmp::Reverse;
use std::sync::{Arc, Mutex};

use hob_core::{CommandResult, Context};
use kettle_ui::widget::*;
use kettle_ui::{Inputs, IslandBuilder, Outcome, Value};

use crate::db::{Db, Note};
use crate::{Filter, count_notes, describe, details, heading};

/// How many matches are listed.
const RESULTS: usize = 5;

/// Searches the notes `filter` lets through, starting with `query`. "Show"
/// (Enter) prints the top match as `notes show` does, and "Edit" edits it
/// as `notes edit` does.
pub(crate) fn search(ctx: &mut Context, db: Db, filter: Filter, query: &str) -> CommandResult {
    if filter.notes(&db)?.is_empty() {
        ctx.print(filter.none_message(&db)?);
        return Ok(());
    }
    let mut island = IslandBuilder::new(column([
        text_field("query", "Search"),
        label(state("count")),
        list("results"),
        label(state("heading")),
        label(state("details")),
        when("has_categories", label(state("categories"))),
        label(state("text")),
        row([
            submit_button("Show"),
            button("edit", "Edit [Alt+e]"),
            cancel_button("Close"),
        ]),
    ]));
    island.set_text("query", query);

    // The matches' ids (as text, being a list), or an error message. The
    // rest is derived from them, so the notes are searched once per change.
    let db = Arc::new(Mutex::new(db));
    let filter = Arc::new(filter);
    let (d, f) = (db.clone(), filter.clone());
    island.derive("found", move |inputs| {
        match matches(&d.lock().unwrap(), &f, inputs.text("query")) {
            Ok(notes) => (notes.iter().map(|note| note.id.to_string()))
                .collect::<Vec<_>>()
                .into(),
            Err(error) => format!("error: {error}").into(),
        }
    });
    island.derive_with_state("count", &["found"], |inputs| {
        match inputs.state("found") {
            Some(Value::List(ids)) => match ids.len() {
                0 => "No notes match.".to_owned(),
                1 => "1 note matches".to_owned(),
                n if n <= RESULTS => format!("{} match", count_notes(n as i64)),
                n => format!("{} match; the first {RESULTS}:", count_notes(n as i64)),
            },
            Some(Value::Text(error)) => error.clone(),
            _ => unreachable!("found is a list or text"),
        }
        .into()
    });
    let d = db.clone();
    island.derive_with_state("results", &["found"], move |inputs| {
        let db = d.lock().unwrap();
        (found(inputs).iter().take(RESULTS))
            .filter_map(|&id| db.get(id).ok().flatten())
            .map(|note| describe(&note))
            .collect::<Vec<_>>()
            .into()
    });
    // The preview of the top match: each piece of state, and how it's made.
    type Piece = (&'static str, fn(&Note) -> Value);
    let preview: [Piece; 5] = [
        ("heading", |note| heading(note).into()),
        ("details", |note| details(note).into()),
        ("has_categories", |note| {
            (!note.categories.is_empty()).into()
        }),
        ("categories", |note| {
            format!("Categories: {}", note.categories.join(", ")).into()
        }),
        ("text", |note| note.text.clone().into()),
    ];
    for (name, show) in preview {
        let d = db.clone();
        island.derive_with_state(name, &["found"], move |inputs| {
            let top = found(inputs).first().copied();
            match top.and_then(|id| d.lock().unwrap().get(id).ok().flatten()) {
                Some(note) => show(&note),
                None if name == "has_categories" => false.into(),
                None => "".into(),
            }
        });
    }

    let (edit, fields) = match ctx.show(island)? {
        Outcome::Submitted(fields) => (false, fields),
        Outcome::Pressed(id, fields) if &*id == "edit" => (true, fields),
        // Closing is all there is to do, so it says nothing.
        Outcome::Cancelled | Outcome::Pressed(..) => return Ok(()),
    };
    // Searched again: notes may have changed since the last keystroke.
    let mut db = db.lock().unwrap();
    match matches(&db, &filter, fields.text("query"))?.first() {
        Some(note) if edit => crate::edit(ctx, &mut db, note),
        Some(note) => crate::show(ctx, note),
        None => {
            ctx.print("No notes match.");
            Ok(())
        }
    }
}

/// The ids in the `found` state, none if it's an error.
fn found(inputs: &Inputs<'_>) -> Vec<i64> {
    match inputs.state("found") {
        Some(Value::List(ids)) => ids.iter().filter_map(|id| id.parse().ok()).collect(),
        _ => Vec::new(),
    }
}

/// The notes `filter` lets through that have every word of `query` in
/// their title or text, ignoring case: those with all of them in the title
/// first, then the newest first. An empty query matches every note.
fn matches(db: &Db, filter: &Filter, query: &str) -> rusqlite::Result<Vec<Note>> {
    let words: Vec<_> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut found: Vec<_> = (filter.notes(db)?.into_iter())
        .filter_map(|note| {
            let title = note.title.as_deref().unwrap_or("").to_lowercase();
            let text = note.text.to_lowercase();
            let in_title = words.iter().all(|word| title.contains(word));
            let matched = words
                .iter()
                .all(|word| title.contains(word) || text.contains(word));
            matched.then_some((in_title, note))
        })
        .collect();
    found.sort_by_key(|(in_title, note)| (!in_title, Reverse(note.id)));
    Ok(found.into_iter().map(|(_, note)| note).collect())
}
