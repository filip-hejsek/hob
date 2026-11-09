//! The notes database: one SQLite file, opened by each command that needs it.

use std::path::Path;
use std::time::Duration;

use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};

use crate::tag::{self, Tag};

/// A command's error.
pub(crate) type Error = Box<dyn std::error::Error + Send + Sync>;

/// The schema version (SQLite's `user_version`) this code reads and writes.
const VERSION: i32 = 4;

/// The notes table, as of version 2. A note's `status` matters only for
/// TODO notes; at most one note is active. Ids aren't reused
/// (`AUTOINCREMENT`), so an id in the scrollback never means another note
/// later.
const NOTES: &str = "
CREATE TABLE notes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    title TEXT,
    text TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'active', 'done')),
    created TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    done TEXT
);
CREATE UNIQUE INDEX notes_one_active ON notes (status) WHERE status = 'active';
";

/// Version 3 adds categories. Names are unique in any ASCII case. Deleting
/// a note or a category deletes its links (foreign keys are turned on for
/// each connection).
const CATEGORIES: &str = "
CREATE TABLE categories (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE COLLATE NOCASE
);
CREATE TABLE note_categories (
    note INTEGER NOT NULL REFERENCES notes (id) ON DELETE CASCADE,
    category INTEGER NOT NULL REFERENCES categories (id) ON DELETE CASCADE,
    PRIMARY KEY (note, category)
) WITHOUT ROWID;
CREATE INDEX note_categories_category ON note_categories (category);
";

/// Version 4 adds tags, by name. Notes made before get theirs from
/// [`migrate_tags`].
const TAGS: &str = "ALTER TABLE notes ADD COLUMN tag TEXT;";

/// Version 1 to 2: rebuilds the table with `AUTOINCREMENT`, keeping
/// everything (ids too). Its index goes first, as index names are global.
const MIGRATE_1: &str = "
DROP INDEX notes_one_active;
ALTER TABLE notes RENAME TO notes_v1;
";

/// The second half of [`MIGRATE_1`], after the new table is made.
const MIGRATE_1_COPY: &str = "
INSERT INTO notes (id, title, text, status, created, done)
    SELECT id, title, text, status, created, done FROM notes_v1;
DROP TABLE notes_v1;
";

/// The columns [`Note::from_row`] reads, from `notes`. Times are shown in
/// local time; category names are joined with newlines, which they can't
/// contain.
const COLUMNS: &str = "id, title, text, tag, status, \
    strftime('%Y-%m-%d %H:%M', created, 'localtime'), \
    strftime('%Y-%m-%d %H:%M', done, 'localtime'), \
    (SELECT group_concat(c.name, char(10) ORDER BY c.name) \
        FROM note_categories nc JOIN categories c ON c.id = nc.category \
        WHERE nc.note = notes.id)";

/// A note's status. Only TODO notes are meant to be active or done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    Open,
    Active,
    Done,
}

impl Status {
    fn sql(self) -> &'static str {
        match self {
            Status::Open => "open",
            Status::Active => "active",
            Status::Done => "done",
        }
    }
}

/// A stored note.
#[derive(Clone, Debug)]
pub(crate) struct Note {
    pub(crate) id: i64,
    /// `None` until it's named.
    pub(crate) title: Option<String>,
    pub(crate) text: String,
    /// Not part of the title or text.
    pub(crate) tag: Option<Tag>,
    pub(crate) status: Status,
    /// When it was made, in local time (`YYYY-MM-DD HH:MM`).
    pub(crate) created: String,
    /// When it was done, like `created`.
    pub(crate) done: Option<String>,
    /// The names of its categories, sorted.
    pub(crate) categories: Vec<String>,
}

impl Note {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        let tag = match row.get::<_, Option<String>>(3)? {
            Some(name) => Some(Tag::from_name(&name).ok_or_else(|| {
                let error = format!("unknown tag {name:?}");
                rusqlite::Error::FromSqlConversionFailure(3, Type::Text, error.into())
            })?),
            None => None,
        };
        let status: String = row.get(4)?;
        Ok(Note {
            id: row.get(0)?,
            title: row.get(1)?,
            text: row.get(2)?,
            tag,
            status: match status.as_str() {
                "active" => Status::Active,
                "done" => Status::Done,
                _ => Status::Open,
            },
            created: row.get(5)?,
            done: row.get(6)?,
            categories: (row.get::<_, Option<String>>(7)?)
                .map(|names| names.split('\n').map(str::to_owned).collect())
                .unwrap_or_default(),
        })
    }

    /// Whether it's a TODO note: it has the tag `todo`.
    pub(crate) fn is_todo(&self) -> bool {
        self.tag == Some(Tag::Todo)
    }
}

/// Whether `text` starts with the word "todo", as TODO notes did before
/// version 4: "TODO: x", "todo x", "todo-x" and "todo" do, "todos" doesn't.
fn starts_with_todo(text: &str) -> bool {
    let Some(rest) = text.get(4..) else {
        return false;
    };
    text[..4].eq_ignore_ascii_case("todo") && !rest.starts_with(char::is_alphanumeric)
}

/// A category notes can be in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Category {
    pub(crate) id: i64,
    pub(crate) name: String,
}

/// A change to a note, made by [`Db::change`].
pub(crate) enum Change {
    /// Names it, and sets its tag if one is given (keeping the old one
    /// otherwise).
    Title { title: String, tag: Option<Tag> },
    /// Replaces its title, text, tag and categories (by id; ids of
    /// categories deleted meanwhile are left out).
    Edit {
        title: Option<String>,
        text: String,
        tag: Option<Tag>,
        categories: Vec<i64>,
    },
    /// Sets its status. Making it active puts the one active before back
    /// to open; making it done records the time, other statuses clear it.
    Status(Status),
    /// Removes it, for good.
    Delete,
}

/// An open notes database.
pub(crate) struct Db(Connection);

impl Db {
    /// Opens the database, creating it (and its directory) if needed.
    pub(crate) fn open(path: &Path) -> Result<Self, String> {
        let fail = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| fail(&e))?;
        }
        let mut conn = Connection::open(path).map_err(|e| fail(&e))?;
        // Commands run concurrently, each with its own connection.
        conn.busy_timeout(Duration::from_secs(5))
            .map_err(|e| fail(&e))?;
        // Off by default, and only settable outside transactions.
        conn.pragma_update(None, "foreign_keys", true)
            .map_err(|e| fail(&e))?;
        migrate(&mut conn).map_err(|e| fail(&e))?;
        Ok(Db(conn))
    }

    /// Adds a note, in the categories given (by id); returns its id.
    pub(crate) fn add(
        &mut self,
        title: Option<&str>,
        text: &str,
        tag: Option<Tag>,
        categories: &[i64],
    ) -> rusqlite::Result<i64> {
        let tx = self.0.transaction()?;
        tx.execute(
            "INSERT INTO notes (title, text, tag) VALUES (?1, ?2, ?3)",
            params![title, text, tag.map(Tag::name)],
        )?;
        let id = tx.last_insert_rowid();
        set_categories(&tx, id, categories)?;
        tx.commit()?;
        Ok(id)
    }

    /// The note with this id, if there is one.
    pub(crate) fn get(&self, id: i64) -> rusqlite::Result<Option<Note>> {
        get(&self.0, id)
    }

    /// The note created last.
    pub(crate) fn last(&self) -> rusqlite::Result<Option<Note>> {
        self.one(&format!(
            "SELECT {COLUMNS} FROM notes ORDER BY id DESC LIMIT 1"
        ))
    }

    /// The active note, if any.
    pub(crate) fn active(&self) -> rusqlite::Result<Option<Note>> {
        self.one(&format!(
            "SELECT {COLUMNS} FROM notes WHERE status = 'active'"
        ))
    }

    /// All notes, oldest first.
    pub(crate) fn all(&self) -> rusqlite::Result<Vec<Note>> {
        self.many(&format!("SELECT {COLUMNS} FROM notes ORDER BY id"))
    }

    /// Open TODO notes, oldest first.
    pub(crate) fn open_todos(&self) -> rusqlite::Result<Vec<Note>> {
        let notes = self.many(&format!(
            "SELECT {COLUMNS} FROM notes WHERE status = 'open' ORDER BY id"
        ))?;
        Ok(notes.into_iter().filter(Note::is_todo).collect())
    }

    /// Unnamed notes that aren't TODO notes (their text says what they
    /// are), oldest first.
    pub(crate) fn unnamed(&self) -> rusqlite::Result<Vec<Note>> {
        let notes = self.many(&format!(
            "SELECT {COLUMNS} FROM notes WHERE title IS NULL ORDER BY id"
        ))?;
        Ok(notes.into_iter().filter(|n| !n.is_todo()).collect())
    }

    /// Changes note `id`, checking first what's stored now rather than what
    /// was read before: `check` gets the note (`None` if there's no such
    /// note), and says what to change, or why not. Both happen in one
    /// transaction. Returns the note as it was.
    pub(crate) fn change(
        &mut self,
        id: i64,
        check: impl FnOnce(Option<&Note>) -> Result<Change, String>,
    ) -> Result<Note, Error> {
        let tx = (self.0).transaction_with_behavior(TransactionBehavior::Immediate)?;
        let note = get(&tx, id)?;
        let change = check(note.as_ref())?;
        let note = note.ok_or_else(|| format!("there's no note #{id}"))?;
        apply(&tx, id, change)?;
        tx.commit()?;
        Ok(note)
    }

    /// All categories, by name.
    pub(crate) fn categories(&self) -> rusqlite::Result<Vec<Category>> {
        let mut statement = (self.0).prepare("SELECT id, name FROM categories ORDER BY name")?;
        let categories = statement.query_map([], Category::from_row)?;
        categories.collect()
    }

    /// The category with this name, in any ASCII case.
    pub(crate) fn category(&self, name: &str) -> rusqlite::Result<Option<Category>> {
        category(&self.0, name)
    }

    /// How many notes are in category `id`.
    pub(crate) fn category_size(&self, id: i64) -> rusqlite::Result<i64> {
        (self.0).query_row(
            "SELECT count(*) FROM note_categories WHERE category = ?1",
            [id],
            |row| row.get(0),
        )
    }

    /// Adds categories, all or none; fails if one exists already.
    pub(crate) fn add_categories(&mut self, names: &[String]) -> Result<(), Error> {
        let tx = (self.0).transaction_with_behavior(TransactionBehavior::Immediate)?;
        for name in names {
            if let Some(old) = category(&tx, name)? {
                return Err(format!("there's already a category {:?}", old.name).into());
            }
            tx.execute("INSERT INTO categories (name) VALUES (?1)", [name])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Renames category `id`; fails if another one has the name, or it was
    /// deleted.
    pub(crate) fn rename_category(&mut self, id: i64, name: &str) -> Result<(), Error> {
        let tx = (self.0).transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(old) = category(&tx, name)?
            && old.id != id
        {
            return Err(format!("there's already a category {:?}", old.name).into());
        }
        let changed = tx.execute(
            "UPDATE categories SET name = ?2 WHERE id = ?1",
            params![id, name],
        )?;
        if changed == 0 {
            return Err("the category was deleted meanwhile".into());
        }
        tx.commit()?;
        Ok(())
    }

    /// Deletes category `id`, taking it off its notes; returns whether it
    /// was there.
    pub(crate) fn delete_category(&self, id: i64) -> rusqlite::Result<bool> {
        Ok((self.0).execute("DELETE FROM categories WHERE id = ?1", [id])? > 0)
    }

    fn one(&self, sql: &str) -> rusqlite::Result<Option<Note>> {
        self.0.query_row(sql, [], Note::from_row).optional()
    }

    fn many(&self, sql: &str) -> rusqlite::Result<Vec<Note>> {
        let mut statement = self.0.prepare(sql)?;
        let notes = statement.query_map([], Note::from_row)?;
        notes.collect()
    }
}

fn get(conn: &Connection, id: i64) -> rusqlite::Result<Option<Note>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM notes WHERE id = ?1"),
        [id],
        Note::from_row,
    )
    .optional()
}

impl Category {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Category {
            id: row.get(0)?,
            name: row.get(1)?,
        })
    }
}

fn category(conn: &Connection, name: &str) -> rusqlite::Result<Option<Category>> {
    conn.query_row(
        "SELECT id, name FROM categories WHERE name = ?1",
        [name],
        Category::from_row,
    )
    .optional()
}

/// Puts note `id` in exactly these categories, leaving out ids that don't
/// exist.
fn set_categories(tx: &Transaction<'_>, id: i64, categories: &[i64]) -> rusqlite::Result<()> {
    tx.execute("DELETE FROM note_categories WHERE note = ?1", [id])?;
    for category in categories {
        tx.execute(
            "INSERT OR IGNORE INTO note_categories (note, category) \
                SELECT ?1, id FROM categories WHERE id = ?2",
            [id, *category],
        )?;
    }
    Ok(())
}

fn apply(tx: &Transaction<'_>, id: i64, change: Change) -> rusqlite::Result<()> {
    match change {
        Change::Title { title, tag } => {
            tx.execute(
                "UPDATE notes SET title = ?2, tag = coalesce(?3, tag) WHERE id = ?1",
                params![id, title, tag.map(Tag::name)],
            )?;
        }
        Change::Edit {
            title,
            text,
            tag,
            categories,
        } => {
            tx.execute(
                "UPDATE notes SET title = ?2, text = ?3, tag = ?4 WHERE id = ?1",
                params![id, title, text, tag.map(Tag::name)],
            )?;
            set_categories(tx, id, &categories)?;
        }
        Change::Status(status) => {
            if status == Status::Active {
                tx.execute(
                    "UPDATE notes SET status = 'open' WHERE status = 'active'",
                    [],
                )?;
            }
            let done = match status {
                Status::Done => "strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
                _ => "NULL",
            };
            tx.execute(
                &format!("UPDATE notes SET status = ?2, done = {done} WHERE id = ?1"),
                params![id, status.sql()],
            )?;
        }
        Change::Delete => {
            tx.execute("DELETE FROM notes WHERE id = ?1", [id])?;
        }
    }
    Ok(())
}

/// Creates the tables in a new database, or brings an old one up to date;
/// refuses one from a newer version.
fn migrate(conn: &mut Connection) -> Result<(), String> {
    let tx = (conn.transaction_with_behavior(TransactionBehavior::Immediate))
        .map_err(|e| e.to_string())?;
    let version: i32 = tx
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|e| e.to_string())?;
    let sql = match version {
        VERSION => return Ok(()),
        0 => format!("{NOTES}{CATEGORIES}{TAGS}"),
        1 => format!("{MIGRATE_1}{NOTES}{MIGRATE_1_COPY}{CATEGORIES}{TAGS}"),
        2 => format!("{CATEGORIES}{TAGS}"),
        3 => TAGS.to_owned(),
        _ => {
            return Err(format!(
                "the database has version {version}; this hob knows only up to {VERSION}"
            ));
        }
    };
    tx.execute_batch(&sql)
        .and_then(|()| migrate_tags(&tx))
        .and_then(|()| tx.pragma_update(None, "user_version", VERSION))
        .and_then(|()| tx.commit())
        .map_err(|e| e.to_string())
}

/// Tags the notes that were TODO notes before version 4 (their title or
/// text started with "todo"), and takes the tag out of the title, or else
/// out of the text, where it's written as tags are now. A text that would
/// be left empty keeps it.
fn migrate_tags(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    let notes: Vec<(i64, Option<String>, String)> = {
        let mut statement = tx.prepare("SELECT id, title, text FROM notes")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for (id, mut title, mut text) in notes {
        if !(title.as_deref().is_some_and(starts_with_todo) || starts_with_todo(&text)) {
            continue;
        }
        if let Some((_, rest)) = title.as_deref().and_then(tag::split) {
            title = Some(rest.to_owned()).filter(|t| !t.is_empty());
        } else if let Some((_, rest)) = tag::split(&text)
            && !rest.is_empty()
        {
            text = rest.to_owned();
        }
        tx.execute(
            "UPDATE notes SET title = ?2, text = ?3, tag = ?4 WHERE id = ?1",
            params![id, title, text, Tag::Todo.name()],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> (Db, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("sub/notes.sqlite")).unwrap();
        (db, dir)
    }

    fn ids(notes: Vec<Note>) -> Vec<i64> {
        notes.iter().map(|n| n.id).collect()
    }

    fn set(db: &mut Db, id: i64, change: Change) {
        db.change(id, |_| Ok(change)).unwrap();
    }

    #[test]
    fn one_note_is_active() {
        let (mut db, _dir) = db();
        let a = db.add(None, "a", Some(Tag::Todo), &[]).unwrap();
        let b = db.add(None, "b", Some(Tag::Todo), &[]).unwrap();
        set(&mut db, a, Change::Status(Status::Active));
        set(&mut db, b, Change::Status(Status::Active));
        assert_eq!(db.active().unwrap().unwrap().id, b);
        assert_eq!(ids(db.open_todos().unwrap()), [a]);
        set(&mut db, b, Change::Status(Status::Done));
        assert!(db.active().unwrap().is_none());
        assert_eq!(ids(db.open_todos().unwrap()), [a]);
        let b = db.get(b).unwrap().unwrap();
        assert_eq!(b.status, Status::Done);
        assert!(b.done.is_some());
        set(&mut db, b.id, Change::Status(Status::Open));
        assert_eq!(db.get(b.id).unwrap().unwrap().done, None);
    }

    #[test]
    fn todo_was_a_word() {
        for text in ["TODO: x", "todo x", "todo", "ToDo", "todo-x", "todo_x"] {
            assert!(starts_with_todo(text), "{text}");
        }
        for text in [
            "todos", "todoist", "todo2", "todoé", "tod", "", "a todo", "tödo",
        ] {
            assert!(!starts_with_todo(text), "{text}");
        }
    }

    #[test]
    fn todo_notes_by_tag() {
        let (mut db, _dir) = db();
        let a = db.add(None, "call", Some(Tag::Todo), &[]).unwrap();
        let b = db.add(None, "plain", None, &[]).unwrap();
        let c = db.add(None, "todo: not parsed here", None, &[]).unwrap();
        let title = |tag| Change::Title {
            title: "Later".into(),
            tag,
        };
        set(&mut db, b, title(Some(Tag::Todo)));
        // A title without a tag keeps the old one.
        set(&mut db, b, title(None));
        assert_eq!(db.get(b).unwrap().unwrap().tag, Some(Tag::Todo));
        assert_eq!(ids(db.open_todos().unwrap()), [a, b]);
        assert_eq!(ids(db.unnamed().unwrap()), [c]);
        // An edit sets it exactly.
        let edit = Change::Edit {
            title: None,
            text: "x".into(),
            tag: None,
            categories: Vec::new(),
        };
        set(&mut db, a, edit);
        assert_eq!(ids(db.open_todos().unwrap()), [b]);
    }

    #[test]
    fn unknown_tags_are_errors() {
        let (db, _dir) = db();
        (db.0)
            .execute("INSERT INTO notes (text, tag) VALUES ('x', 'later')", [])
            .unwrap();
        let error = db.all().unwrap_err().to_string();
        assert!(error.contains(r#"unknown tag "later""#), "{error}");
    }

    #[test]
    fn changes_see_what_is_stored() {
        let (mut db, _dir) = db();
        let a = db.add(Some("A"), "text", None, &[]).unwrap();
        set(&mut db, a, Change::Delete);
        let error = db.change(a, |note| {
            assert!(note.is_none());
            Err("gone".into())
        });
        assert_eq!(error.unwrap_err().to_string(), "gone");
        assert!(db.get(a).unwrap().is_none());
    }

    #[test]
    fn ids_are_not_reused() {
        let (mut db, _dir) = db();
        db.add(None, "a", None, &[]).unwrap();
        let b = db.add(None, "b", None, &[]).unwrap();
        set(&mut db, b, Change::Delete);
        assert_eq!(db.add(None, "c", None, &[]).unwrap(), b + 1);
    }

    /// A version 1 database, as that version made it.
    const SCHEMA_1: &str = "
    CREATE TABLE notes (
        id INTEGER PRIMARY KEY,
        title TEXT,
        text TEXT NOT NULL,
        status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'active', 'done')),
        created TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
        done TEXT
    );
    CREATE UNIQUE INDEX notes_one_active ON notes (status) WHERE status = 'active';
    PRAGMA user_version = 1;
    INSERT INTO notes (id, title, text, status, created, done) VALUES
        (1, NULL, 'todo first', 'done', '2026-01-01T10:00:00Z', '2026-01-02T10:00:00Z'),
        (2, 'Two', 'todo second', 'active', '2026-01-03T10:00:00Z', NULL),
        (5, NULL, 'third', 'open', '2026-01-04T10:00:00Z', NULL);
    ";

    #[test]
    fn migrates_version_1() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.sqlite");
        Connection::open(&path)
            .unwrap()
            .execute_batch(SCHEMA_1)
            .unwrap();
        let mut db = Db::open(&path).unwrap();
        let notes = db.all().unwrap();
        assert_eq!(ids(notes.clone()), [1, 2, 5]);
        assert_eq!(notes[0].status, Status::Done);
        assert!(notes[0].done.is_some());
        assert_eq!(notes[1].title.as_deref(), Some("Two"));
        assert_eq!(notes[2].text, "third");
        // TODO notes got their tag.
        assert_eq!(
            (notes[0].text.as_str(), notes[0].tag),
            ("first", Some(Tag::Todo))
        );
        assert_eq!(
            (notes[1].text.as_str(), notes[1].tag),
            ("second", Some(Tag::Todo))
        );
        assert_eq!(notes[2].tag, None);
        assert_eq!(db.active().unwrap().unwrap().id, 2);
        let version: i32 = (db.0)
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, VERSION);
        // Still one active note at most.
        assert!(
            (db.0)
                .execute("UPDATE notes SET status = 'active' WHERE id = 1", [])
                .is_err()
        );
        // Ids aren't reused, even of the last note deleted.
        set(&mut db, 5, Change::Delete);
        assert_eq!(db.add(None, "new", None, &[]).unwrap(), 6);
        drop(db);
        // Opening it again changes nothing.
        let db = Db::open(&path).unwrap();
        assert_eq!(ids(db.all().unwrap()), [1, 2, 6]);
    }

    #[test]
    fn migrates_version_2() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(NOTES).unwrap();
        conn.execute_batch(
            "PRAGMA user_version = 2;
            INSERT INTO notes (id, text) VALUES (3, 'kept');",
        )
        .unwrap();
        drop(conn);
        let mut db = Db::open(&path).unwrap();
        assert_eq!(db.get(3).unwrap().unwrap().text, "kept");
        db.add_categories(&["a".into()]).unwrap();
        let a = db.category("A").unwrap().unwrap().id;
        let id = db.add(None, "new", None, &[a]).unwrap();
        assert_eq!(id, 4);
        assert_eq!(db.get(id).unwrap().unwrap().categories, ["a"]);
    }

    #[test]
    fn migrates_version_3() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(&format!("{NOTES}{CATEGORIES}")).unwrap();
        conn.execute_batch(
            "PRAGMA user_version = 3;
            INSERT INTO notes (id, title, text) VALUES
                (1, 'TODO: Call', 'todo: then write'),
                (2, 'todo', 'only the title'),
                (3, 'Named', 'Todo  in the text'),
                (4, NULL, 'todo'),
                (5, NULL, 'todo-x is old style'),
                (6, NULL, 'todos'),
                (7, 'Plain', 'text');",
        )
        .unwrap();
        drop(conn);
        let db = Db::open(&path).unwrap();
        let notes: Vec<_> = (db.all().unwrap().into_iter())
            .map(|n| (n.title, n.text, n.tag.is_some()))
            .collect();
        let note = |title: Option<&str>, text: &str, todo| {
            (title.map(str::to_owned), text.to_owned(), todo)
        };
        assert_eq!(
            notes,
            [
                // The tag goes from the title only.
                note(Some("Call"), "todo: then write", true),
                note(None, "only the title", true),
                note(Some("Named"), "in the text", true),
                // A text left empty, or not a tag now, is kept.
                note(None, "todo", true),
                note(None, "todo-x is old style", true),
                note(None, "todos", false),
                note(Some("Plain"), "text", false),
            ]
        );
    }

    #[test]
    fn deleting_takes_notes_out_of_categories() {
        let (mut db, _dir) = db();
        db.add_categories(&["b".into(), "A".into()]).unwrap();
        let ids: Vec<_> = db.categories().unwrap().iter().map(|c| c.id).collect();
        let note = db.add(None, "x", None, &[ids[0], ids[1], 99]).unwrap();
        // Sorted in any case; the unknown id is left out.
        assert_eq!(db.get(note).unwrap().unwrap().categories, ["A", "b"]);
        assert!(db.delete_category(ids[0]).unwrap());
        assert_eq!(db.get(note).unwrap().unwrap().categories, ["b"]);
        assert_eq!(db.category_size(ids[1]).unwrap(), 1);
        set(&mut db, note, Change::Delete);
        assert_eq!(db.category_size(ids[1]).unwrap(), 0);
    }

    #[test]
    fn refuses_newer_versions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", VERSION + 1)
            .unwrap();
        drop(conn);
        let error = Db::open(&path).err().unwrap();
        assert!(
            error.contains(&format!("version {}", VERSION + 1)),
            "{error}"
        );
    }
}
