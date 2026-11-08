//! Note-taking commands for hob: `note`, `title`, `todo` and `done`.
//!
//! Notes are kept in an SQLite database. Each is created unnamed; `title`
//! names the last one. TODO items aren't stored: `todo` makes them from the
//! notes each time (TODO notes to work on, unnamed notes to name), and
//! offers a few.

mod db;
mod todo;

use std::path::PathBuf;
use std::sync::Arc;

use hob_core::args::Spec;
use hob_core::{CommandResult, Context, Interpreter};
use kettle_ui::widget::*;
use kettle_ui::{IslandBuilder, Outcome};

use crate::db::{Db, Note};
use crate::todo::Item;

/// How many items `todo` offers.
const TODO_ITEMS: usize = 3;

/// Keys of the `todo` items' buttons, in order.
const ITEM_KEYS: [char; TODO_ITEMS] = ['a', 'b', 'c'];

/// The longest a note's text is shown in a one-line description.
const PREVIEW_CHARS: usize = 50;

/// Registers the note commands, with the database at `path`. Without a path
/// (no place for it was found), they fail.
pub fn register(interpreter: &mut Interpreter, path: Option<PathBuf>) {
    let path = Arc::new(path);
    let open = move || match path.as_deref() {
        Some(path) => Db::open(path),
        None => Err("there's no place for the notes database".to_owned()),
    };
    let o = open.clone();
    interpreter.register(
        "note",
        "Take a note (the arguments, or asks for it)",
        move |ctx, args| note(ctx, args, o()?),
    );
    let o = open.clone();
    interpreter.register(
        "title",
        "Name the last note (the arguments, or asks for it)",
        move |ctx, args| title(ctx, args, o()?),
    );
    let o = open.clone();
    interpreter.register_spec(
        "todo",
        "Pick something to do now",
        Spec::new(),
        move |ctx, _| todo(ctx, o()?),
    );
    interpreter.register_spec(
        "done",
        "Mark the active TODO note as done",
        Spec::new(),
        move |ctx, _| done(ctx, open()?),
    );
}

/// Saves a note. The database is opened before asking, so a broken one
/// doesn't lose what the user typed.
fn note(ctx: &mut Context, args: &[String], db: Db) -> CommandResult {
    let text = if args.is_empty() {
        let island = IslandBuilder::new(column([
            text_area("text", "Note"),
            row([submit_button("Save"), cancel_button("Cancel")]),
        ]));
        match ctx.show(island)? {
            Outcome::Submitted(fields) => fields.text("text").to_owned(),
            Outcome::Cancelled | Outcome::Pressed(..) => {
                ctx.print("Not saved.");
                return Ok(());
            }
        }
    } else {
        args.join(" ")
    };
    let text = text.trim();
    if text.is_empty() {
        return Err("the note is empty".into());
    }
    let id = db.add(text)?;
    ctx.print(format!("Saved note #{id}."));
    Ok(())
}

/// Names the last note.
fn title(ctx: &mut Context, args: &[String], db: Db) -> CommandResult {
    let note = db.last()?.ok_or("there are no notes yet")?;
    if args.is_empty() {
        ask_title(ctx, &db, &note)
    } else {
        set_title(ctx, &db, &note, &args.join(" "))
    }
}

/// Asks for a note's name, showing the old one to edit.
fn ask_title(ctx: &mut Context, db: &Db, note: &Note) -> CommandResult {
    let mut island = IslandBuilder::new(column([
        label(format!("Name {}", describe(note))),
        text_field("title", "Title"),
        row([submit_button("Save"), cancel_button("Cancel")]),
    ]));
    island.set_text("title", note.title.as_deref().unwrap_or(""));
    match ctx.show(island)? {
        Outcome::Submitted(fields) => set_title(ctx, db, note, fields.text("title")),
        Outcome::Cancelled | Outcome::Pressed(..) => {
            ctx.print("Not named.");
            Ok(())
        }
    }
}

fn set_title(ctx: &mut Context, db: &Db, note: &Note, title: &str) -> CommandResult {
    let title = title.trim();
    if title.is_empty() {
        return Err("the title is empty".into());
    }
    db.set_title(note.id, title)?;
    ctx.print(format!("Named note #{}: {title}", note.id));
    Ok(())
}

/// Offers a few TODO items; acts on the one picked.
fn todo(ctx: &mut Context, mut db: Db) -> CommandResult {
    let items = todo::pick(&db, TODO_ITEMS)?;
    let active = db.active()?;
    if items.is_empty() {
        let message = match &active {
            Some(note) => format!("Nothing else to do; working on {}.", describe(note)),
            None => "Nothing to do.".to_owned(),
        };
        ctx.print(message);
        return Ok(());
    }
    let mut children = Vec::new();
    if let Some(note) = &active {
        children.push(label(format!("Working on {}", describe(note))));
    }
    children.push(label("Pick something to do:"));
    for (i, (item, key)) in items.iter().zip(ITEM_KEYS).enumerate() {
        children.push(button(
            &i.to_string(),
            ButtonLabel::new(item.label()).key(key),
        ));
    }
    children.push(cancel_button("Not now"));
    let Outcome::Pressed(id, _) = ctx.show(IslandBuilder::new(column(children)))? else {
        return Ok(());
    };
    let item = id
        .parse()
        .ok()
        .and_then(|i: usize| items.get(i))
        .ok_or_else(|| format!("no item {id:?}"))?;
    match item {
        Item::Name(note) => ask_title(ctx, &db, note),
        Item::Do(note) => {
            db.activate(note.id)?;
            ctx.print(format!(
                "Working on {}; run done when it's done.",
                describe(note)
            ));
            Ok(())
        }
    }
}

/// Marks the active note as done.
fn done(ctx: &mut Context, db: Db) -> CommandResult {
    let note = db
        .active()?
        .ok_or("nothing is active; pick a TODO note with todo")?;
    db.finish(note.id)?;
    ctx.print(format!("Done: {}", describe(&note)));
    Ok(())
}

/// A note in one line: its id and title, or the start of its text.
fn describe(note: &Note) -> String {
    match &note.title {
        Some(title) => format!("#{}: {title}", note.id),
        None => format!("#{}: {}", note.id, preview(&note.text)),
    }
}

/// The first line of `text`, cut short with "…" if it's long or there's
/// more.
fn preview(text: &str) -> String {
    let mut lines = text.lines();
    let first = lines.next().unwrap_or("");
    let mut preview: String = first.chars().take(PREVIEW_CHARS).collect();
    if preview.len() < first.len() || lines.next().is_some() {
        preview.push('…');
    }
    preview
}
