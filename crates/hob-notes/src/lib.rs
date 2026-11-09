//! Note-taking commands for hob: `note`, `title`, `todo`, `done` and
//! `notes`.
//!
//! Notes are kept in an SQLite database. `note` takes one, `title` names
//! one, and `notes` lists, shows, edits, deletes or reopens them. TODO
//! items aren't stored: `todo` makes them from the notes each time (TODO
//! notes to work on, unnamed notes to name), and offers a few; `done`
//! finishes the one picked.

mod db;
mod todo;

use std::path::PathBuf;
use std::sync::Arc;

use hob_core::args::{Kind, Matches, Spec};
use hob_core::{CommandResult, Context, Interpreter};
use kettle_ui::widget::*;
use kettle_ui::{IslandBuilder, Outcome};

use crate::db::{Change, Db, Note, Status};
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
    let id = || Spec::new().arg("ID", Kind::Text);
    let o = open.clone();
    interpreter.register_spec(
        "note",
        "Take a note (the arguments, or asks for it)",
        // Free text: `note buy -2 eggs`.
        Spec::new()
            .arg("TEXT", Kind::Text)
            .optional()
            .repeated()
            .no_options(),
        move |ctx, args| note(ctx, args, o()?),
    );
    let o = open.clone();
    interpreter.register_spec(
        "title",
        "Name the last note (the arguments, or asks for it)",
        Spec::new()
            .option("note", "ID", Kind::Text, "Name note ID instead")
            .short('n')
            .arg("TITLE", Kind::Text)
            .optional()
            .repeated(),
        move |ctx, args| title(ctx, args, o()?),
    );
    let o = open.clone();
    interpreter.register_spec(
        "todo",
        "Pick something to do now",
        Spec::new(),
        move |ctx, _| todo(ctx, o()?),
    );
    let o = open.clone();
    interpreter.register_spec(
        "done",
        "Mark the active TODO note, or the one given, as done",
        id().optional(),
        move |ctx, args| done(ctx, args, o()?),
    );
    let spec = Spec::new()
        .subcommand(
            "list",
            "List the notes, oldest first (the default)",
            Spec::new()
                .flag("all", "Also list the notes that are done")
                .flag("todo", "List only TODO notes"),
        )
        .subcommand("show", "Show a note in full", id())
        .subcommand("edit", "Edit a note's title and text", id())
        .subcommand(
            "delete",
            "Delete a note, for good",
            Spec::new()
                .flag("yes", "Don't ask first")
                .short('y')
                .arg("ID", Kind::Text),
        )
        .subcommand("reopen", "Make a done or active TODO note open again", id())
        .optional();
    interpreter.register_spec(
        "notes",
        "List, show, edit or delete notes",
        spec,
        move |ctx, args| notes(ctx, args, open()?),
    );
}

/// Saves a note. The database is opened before asking, so a broken one
/// doesn't lose what the user typed.
fn note(ctx: &mut Context, args: &Matches, db: Db) -> CommandResult {
    let words = args.values("TEXT");
    let (title, text) = if words.is_empty() {
        let island = IslandBuilder::new(column([
            text_field("title", "Title (optional)"),
            text_area("text", "Note"),
            row([submit_button("Save"), cancel_button("Cancel")]),
        ]));
        match ctx.show(island)? {
            Outcome::Submitted(fields) => (
                optional(fields.text("title")),
                fields.text("text").to_owned(),
            ),
            Outcome::Cancelled | Outcome::Pressed(..) => {
                ctx.print("Not saved.");
                return Ok(());
            }
        }
    } else {
        (None, words.join(" "))
    };
    let text = text.trim();
    if text.is_empty() {
        return Err("the note is empty".into());
    }
    let id = db.add(title.as_deref(), text)?;
    ctx.print(format!("Saved note #{id}."));
    Ok(())
}

/// Names the last note, or the one given.
fn title(ctx: &mut Context, args: &Matches, mut db: Db) -> CommandResult {
    let note = match args.value("note") {
        Some(id) => existing(&db, parse_id(id)?)?,
        None => db.last()?.ok_or("there are no notes yet")?,
    };
    let words = args.values("TITLE");
    if words.is_empty() {
        ask_title(ctx, &mut db, &note)
    } else {
        set_title(ctx, &mut db, note.id, &words.join(" "), no_note)
    }
}

/// Asks for a note's name, showing the old one to edit.
fn ask_title(ctx: &mut Context, db: &mut Db, note: &Note) -> CommandResult {
    let mut island = IslandBuilder::new(column([
        label(format!("Name {}", describe(note))),
        text_field("title", "Title"),
        row([submit_button("Save"), cancel_button("Cancel")]),
    ]));
    island.set_text("title", note.title.as_deref().unwrap_or(""));
    match ctx.show(island)? {
        Outcome::Submitted(fields) => set_title(ctx, db, note.id, fields.text("title"), deleted),
        Outcome::Cancelled | Outcome::Pressed(..) => {
            ctx.print("Not named.");
            Ok(())
        }
    }
}

/// Names note `id`; `missing` says why it can't if there's no such note.
fn set_title(
    ctx: &mut Context,
    db: &mut Db,
    id: i64,
    title: &str,
    missing: fn(i64) -> String,
) -> CommandResult {
    let title = title.trim();
    if title.is_empty() {
        return Err("the title is empty".into());
    }
    db.change(id, |note| {
        note.ok_or_else(|| missing(id))?;
        Ok(Change::Title(Some(title.to_owned())))
    })?;
    ctx.print(format!("Named note #{id}: {title}"));
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
        ctx.print("Nothing picked.");
        return Ok(());
    };
    let item = id
        .parse()
        .ok()
        .and_then(|i: usize| items.get(i))
        .ok_or_else(|| format!("no item {id:?}"))?;
    match item {
        Item::Name(note) => ask_title(ctx, &mut db, note),
        Item::Do(note) => {
            let id = note.id;
            let note = db.change(id, |note| {
                let note = note.ok_or_else(|| deleted(id))?;
                if note.status == Status::Done {
                    return Err(format!("note #{id} was marked done meanwhile"));
                }
                if !note.is_todo() {
                    return Err(format!("note #{id} isn't a TODO note any more"));
                }
                Ok(Change::Status(Status::Active))
            })?;
            ctx.print(format!(
                "Working on {}; run done when it's done.",
                describe(&note)
            ));
            Ok(())
        }
    }
}

/// Marks the active note, or the TODO note given, as done.
fn done(ctx: &mut Context, args: &Matches, mut db: Db) -> CommandResult {
    let id = match args.value("ID") {
        Some(id) => parse_id(id)?,
        None => {
            (db.active()?)
                .ok_or("nothing is active; pick a TODO note with todo, or give its ID")?
                .id
        }
    };
    let note = db.change(id, |note| {
        let note = note.ok_or_else(|| no_note(id))?;
        if !note.is_todo() {
            return Err(format!("#{id} isn't a TODO note"));
        }
        if note.status == Status::Done {
            return Err(format!("#{id} is already done"));
        }
        Ok(Change::Status(Status::Done))
    })?;
    ctx.print(format!("Done: {}", describe(&note)));
    Ok(())
}

/// `notes` and its subcommands.
fn notes(ctx: &mut Context, args: &Matches, mut db: Db) -> CommandResult {
    let Some((subcommand, args)) = args.subcommand() else {
        return list(ctx, &db, false, false);
    };
    let id = match subcommand {
        "list" => return list(ctx, &db, args.flag("all"), args.flag("todo")),
        _ => parse_id(args.value("ID").expect("ID is required"))?,
    };
    match subcommand {
        "show" => show(ctx, &existing(&db, id)?),
        "edit" => {
            let note = existing(&db, id)?;
            edit(ctx, &mut db, &note)
        }
        "delete" => delete(ctx, &mut db, id, args.flag("yes")),
        "reopen" => reopen(ctx, &mut db, id),
        _ => unreachable!("subcommands are declared"),
    }
}

/// Lists the notes in a table: done ones only with `all`, and only TODO
/// notes with `todos`.
fn list(ctx: &mut Context, db: &Db, all: bool, todos: bool) -> CommandResult {
    let rows: Vec<_> = (db.all()?.iter())
        .filter(|note| all || note.status != Status::Done)
        .filter(|note| !todos || note.is_todo())
        .map(|note| {
            let title = match &note.title {
                Some(title) => title.clone(),
                None => preview(&note.text),
            };
            [
                label(format!("#{}", note.id)),
                label(state(note)),
                label(title),
            ]
        })
        .collect();
    if rows.is_empty() {
        ctx.print("No notes.");
        return Ok(());
    }
    ctx.print_island(IslandBuilder::new(table_horizontal(rows)).freeze()?);
    Ok(())
}

/// Prints a note in full: a heading, then its text.
fn show(ctx: &mut Context, note: &Note) -> CommandResult {
    let heading = match &note.title {
        Some(title) => format!("#{}: {title}", note.id),
        None => format!("#{}", note.id),
    };
    ctx.print(format!("{heading}\n{}\n\n{}", details(note), note.text));
    Ok(())
}

/// A note's state and dates, e.g. `done, created 2026-01-02 10:00, done
/// 2026-01-03 09:00`.
fn details(note: &Note) -> String {
    let mut details = Vec::new();
    let state = state(note);
    if !state.is_empty() {
        details.push(state.to_owned());
    }
    details.push(format!("created {}", note.created));
    if let Some(done) = &note.done {
        details.push(format!("done {done}"));
    }
    details.join(", ")
}

/// Asks for a note's new title and text.
fn edit(ctx: &mut Context, db: &mut Db, note: &Note) -> CommandResult {
    let mut island = IslandBuilder::new(column([
        text_field("title", "Title (optional)"),
        text_area("text", "Note"),
        row([submit_button("Save"), cancel_button("Cancel")]),
    ]));
    island.set_text("title", note.title.as_deref().unwrap_or(""));
    island.set_text("text", &note.text);
    let fields = match ctx.show(island)? {
        Outcome::Submitted(fields) => fields,
        Outcome::Cancelled | Outcome::Pressed(..) => {
            ctx.print("Not saved.");
            return Ok(());
        }
    };
    let title = optional(fields.text("title"));
    let text = fields.text("text").trim();
    if text.is_empty() {
        return Err("the note is empty".into());
    }
    let id = note.id;
    db.change(id, |note| {
        note.ok_or_else(|| deleted(id))?;
        Ok(Change::Edit {
            title,
            text: text.to_owned(),
        })
    })?;
    ctx.print(format!("Saved note #{id}."));
    Ok(())
}

/// Deletes a note, after asking unless `yes`.
fn delete(ctx: &mut Context, db: &mut Db, id: i64, yes: bool) -> CommandResult {
    let mut missing: fn(i64) -> String = no_note;
    if !yes {
        let note = existing(db, id)?;
        let island = IslandBuilder::new(column([
            label(format!("Delete {}?", describe(&note))),
            label(details(&note)),
            row([button_danger("delete", "Delete"), cancel_button("Cancel")]),
        ]));
        if !matches!(ctx.show(island)?, Outcome::Pressed(..)) {
            ctx.print("Not deleted.");
            return Ok(());
        }
        missing = deleted;
    }
    db.change(id, |note| {
        note.ok_or_else(|| missing(id))?;
        Ok(Change::Delete)
    })?;
    ctx.print(format!("Deleted note #{id}."));
    Ok(())
}

/// Makes a done or active TODO note open again.
fn reopen(ctx: &mut Context, db: &mut Db, id: i64) -> CommandResult {
    let note = db.change(id, |note| {
        let note = note.ok_or_else(|| no_note(id))?;
        if !note.is_todo() {
            return Err(format!("#{id} isn't a TODO note"));
        }
        if note.status == Status::Open {
            return Err(format!("#{id} is already open"));
        }
        Ok(Change::Status(Status::Open))
    })?;
    ctx.print(format!("Reopened {}", describe(&note)));
    Ok(())
}

/// Parses an ID, written `3` or `#3`.
fn parse_id(id: &str) -> Result<i64, String> {
    (id.strip_prefix('#').unwrap_or(id).parse()).map_err(|_| format!("invalid ID: {id:?}"))
}

/// The note `id`, which must exist.
fn existing(db: &Db, id: i64) -> Result<Note, db::Error> {
    Ok(db.get(id)?.ok_or_else(|| no_note(id))?)
}

/// The error for an ID given that doesn't exist.
fn no_note(id: i64) -> String {
    format!("there's no note #{id}")
}

/// The error for a note that was deleted while an island was shown.
fn deleted(id: i64) -> String {
    format!("note #{id} was deleted meanwhile")
}

/// A trimmed title, or `None` if it's empty.
fn optional(title: &str) -> Option<String> {
    Some(title.trim().to_owned()).filter(|t| !t.is_empty())
}

/// A note's state, as `notes` shows it: `todo`, `active`, `done`, or empty
/// for a plain note.
fn state(note: &Note) -> &'static str {
    match note.status {
        Status::Active => "active",
        Status::Done => "done",
        Status::Open if note.is_todo() => "todo",
        Status::Open => "",
    }
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
