//! Note-taking commands for hob: `note`, `title`, `todo`, `done` and
//! `notes`.
//!
//! Notes are kept in an SQLite database. `note` takes one, `title` names
//! one, and `notes` lists, shows, edits, deletes or reopens them, and
//! manages the categories notes can be in. TODO
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
use kettle_ui::{Fields, IslandBuilder, Outcome};

use crate::db::{Category, Change, Db, Note, Status};
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
        move |ctx, args| note(ctx, args, &mut o()?),
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
                .flag("todo", "List only TODO notes")
                .option(
                    "category",
                    "NAME",
                    Kind::Text,
                    "List only notes in category NAME",
                )
                .short('c'),
        )
        .subcommand("show", "Show a note in full", id())
        .subcommand("edit", "Edit a note's title, text and categories", id())
        .subcommand(
            "delete",
            "Delete a note, for good",
            Spec::new()
                .flag("yes", "Don't ask first")
                .short('y')
                .arg("ID", Kind::Text),
        )
        .subcommand("reopen", "Make a done or active TODO note open again", id())
        .subcommand(
            "categories",
            "List, add, rename or delete categories",
            Spec::new()
                .subcommand(
                    "list",
                    "List the categories, with their notes' count (the default)",
                    Spec::new(),
                )
                .subcommand(
                    "add",
                    "Add categories",
                    Spec::new().arg("NAME", Kind::Text).repeated(),
                )
                .subcommand(
                    "rename",
                    "Rename a category",
                    Spec::new().arg("NAME", Kind::Text).arg("NEW", Kind::Text),
                )
                .subcommand(
                    "delete",
                    "Delete a category, taking it off its notes",
                    Spec::new()
                        .flag("yes", "Don't ask first")
                        .short('y')
                        .arg("NAME", Kind::Text),
                )
                .optional(),
        )
        .optional();
    interpreter.register_spec(
        "notes",
        "List, show, edit or delete notes, and manage categories",
        spec,
        move |ctx, args| notes(ctx, args, open()?),
    );
}

/// Saves a note. The database is opened before asking, so a broken one
/// doesn't lose what the user typed.
fn note(ctx: &mut Context, args: &Matches, db: &mut Db) -> CommandResult {
    let words = args.values("TEXT");
    let (title, text, categories) = if words.is_empty() {
        let categories = db.categories()?;
        match ctx.show(note_island(&categories))? {
            Outcome::Submitted(fields) => (
                optional(fields.text("title")),
                fields.text("text").to_owned(),
                checked(&fields, &categories),
            ),
            Outcome::Cancelled | Outcome::Pressed(..) => {
                ctx.print("Not saved.");
                return Ok(());
            }
        }
    } else {
        (None, words.join(" "), Vec::new())
    };
    let text = text.trim();
    if text.is_empty() {
        return Err("the note is empty".into());
    }
    let id = db.add(title.as_deref(), text, &categories)?;
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
        return list(ctx, &db, Filter::default());
    };
    let id = match subcommand {
        "list" => {
            let filter = Filter {
                all: args.flag("all"),
                todo: args.flag("todo"),
                category: args.value("category"),
            };
            return list(ctx, &db, filter);
        }
        "categories" => return categories(ctx, args, &mut db),
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

/// Which notes `notes list` shows.
#[derive(Default)]
struct Filter<'a> {
    /// Done ones too.
    all: bool,
    /// Only TODO notes.
    todo: bool,
    /// Only those in this category.
    category: Option<&'a str>,
}

/// Lists the notes in a table, as `filter` says. Their categories get a
/// column if any note listed has one.
fn list(ctx: &mut Context, db: &Db, filter: Filter) -> CommandResult {
    let category = match filter.category {
        Some(name) => Some(existing_category(db, name)?.name),
        None => None,
    };
    let notes: Vec<_> = (db.all()?.into_iter())
        .filter(|note| filter.all || note.status != Status::Done)
        .filter(|note| !filter.todo || note.is_todo())
        .filter(|note| {
            category
                .as_ref()
                .is_none_or(|c| note.categories.contains(c))
        })
        .collect();
    if notes.is_empty() {
        ctx.print("No notes.");
        return Ok(());
    }
    let with_categories = notes.iter().any(|note| !note.categories.is_empty());
    let rows = notes.iter().map(|note| {
        let title = match &note.title {
            Some(title) => title.clone(),
            None => preview(&note.text),
        };
        let mut row = vec![
            label(format!("#{}", note.id)),
            label(state(note)),
            label(title),
        ];
        if with_categories {
            row.push(label(note.categories.join(", ")));
        }
        row
    });
    ctx.print_island(IslandBuilder::new(table_horizontal(rows)).freeze()?);
    Ok(())
}

/// Prints a note in full: a heading, details, its categories if it has
/// any, then its text.
fn show(ctx: &mut Context, note: &Note) -> CommandResult {
    let mut head = match &note.title {
        Some(title) => format!("#{}: {title}", note.id),
        None => format!("#{}", note.id),
    };
    head = format!("{head}\n{}", details(note));
    if !note.categories.is_empty() {
        head = format!("{head}\nCategories: {}", note.categories.join(", "));
    }
    ctx.print(format!("{head}\n\n{}", note.text));
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

/// Asks for a note's new title, text and categories.
fn edit(ctx: &mut Context, db: &mut Db, note: &Note) -> CommandResult {
    let categories = db.categories()?;
    let mut island = note_island(&categories);
    island.set_text("title", note.title.as_deref().unwrap_or(""));
    island.set_text("text", &note.text);
    for category in &categories {
        if note.categories.contains(&category.name) {
            island.set_checked(&category_field(category), true);
        }
    }
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
            categories: checked(&fields, &categories),
        })
    })?;
    ctx.print(format!("Saved note #{id}."));
    Ok(())
}

/// The island that asks for a note: title, text, and a checkbox for each
/// category.
fn note_island(categories: &[Category]) -> IslandBuilder {
    let mut children = vec![
        text_field("title", "Title (optional)"),
        text_area("text", "Note"),
    ];
    if !categories.is_empty() {
        children.push(label("Categories:"));
        for category in categories {
            children.push(checkbox(&category_field(category), &category.name));
        }
    }
    children.push(row([submit_button("Save"), cancel_button("Cancel")]));
    IslandBuilder::new(column(children))
}

/// The id of a category's checkbox. Not its name, which may be anything.
fn category_field(category: &Category) -> String {
    format!("category-{}", category.id)
}

/// The ids of the categories checked.
fn checked(fields: &Fields, categories: &[Category]) -> Vec<i64> {
    (categories.iter())
        .filter(|category| fields.checked(&category_field(category)))
        .map(|category| category.id)
        .collect()
}

/// `notes categories` and its subcommands.
fn categories(ctx: &mut Context, args: &Matches, db: &mut Db) -> CommandResult {
    let Some((subcommand, args)) = args.subcommand() else {
        return list_categories(ctx, db);
    };
    match subcommand {
        "list" => list_categories(ctx, db),
        "add" => {
            let names = (args.values("NAME").iter())
                .map(|name| category_name(name))
                .collect::<Result<Vec<_>, _>>()?;
            for (i, name) in names.iter().enumerate() {
                if names[..i].iter().any(|n| n.eq_ignore_ascii_case(name)) {
                    return Err(format!("category {name:?} is given twice").into());
                }
            }
            db.add_categories(&names)?;
            for name in names {
                ctx.print(format!("Added category {name:?}."));
            }
            Ok(())
        }
        "rename" => {
            let old = existing_category(db, args.value("NAME").expect("NAME is required"))?;
            let new = category_name(args.value("NEW").expect("NEW is required"))?;
            db.rename_category(old.id, &new)?;
            ctx.print(format!("Renamed category {:?} to {new:?}.", old.name));
            Ok(())
        }
        "delete" => delete_category(ctx, db, args),
        _ => unreachable!("subcommands are declared"),
    }
}

/// Lists the categories in a table, with how many notes each has.
fn list_categories(ctx: &mut Context, db: &Db) -> CommandResult {
    let categories = db.categories()?;
    if categories.is_empty() {
        ctx.print("No categories.");
        return Ok(());
    }
    let mut rows = Vec::new();
    for category in categories {
        let size = db.category_size(category.id)?;
        rows.push([label(category.name), label(count_notes(size))]);
    }
    ctx.print_island(IslandBuilder::new(table_horizontal(rows)).freeze()?);
    Ok(())
}

/// Deletes a category, after asking unless `--yes`.
fn delete_category(ctx: &mut Context, db: &mut Db, args: &Matches) -> CommandResult {
    let category = existing_category(db, args.value("NAME").expect("NAME is required"))?;
    if !args.flag("yes") {
        let size = db.category_size(category.id)?;
        let island = IslandBuilder::new(column([
            label(format!("Delete category {:?}?", category.name)),
            label(format!("It's taken off {}.", count_notes(size))),
            row([button_danger("delete", "Delete"), cancel_button("Cancel")]),
        ]));
        if !matches!(ctx.show(island)?, Outcome::Pressed(..)) {
            ctx.print("Not deleted.");
            return Ok(());
        }
    }
    if !db.delete_category(category.id)? {
        return Err(format!("category {:?} was deleted meanwhile", category.name).into());
    }
    ctx.print(format!("Deleted category {:?}.", category.name));
    Ok(())
}

/// The category named `name`, which must exist.
fn existing_category(db: &Db, name: &str) -> Result<Category, db::Error> {
    let name = name.trim();
    Ok(db
        .category(name)?
        .ok_or_else(|| format!("there's no category {name:?}"))?)
}

/// A new category name, trimmed. It can't be empty, nor have control
/// characters (the database joins names with newlines).
fn category_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("the category name is empty".into());
    }
    if name.contains(char::is_control) {
        return Err(format!("invalid category name {name:?}"));
    }
    Ok(name.to_owned())
}

/// `no notes`, `1 note`, `2 notes`.
fn count_notes(n: i64) -> String {
    match n {
        0 => "no notes".to_owned(),
        1 => "1 note".to_owned(),
        _ => format!("{n} notes"),
    }
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
