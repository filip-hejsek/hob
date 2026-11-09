//! The notes database: one SQLite file, opened by each command that needs it.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};

/// A command's error.
pub(crate) type Error = Box<dyn std::error::Error + Send + Sync>;

/// The schema version (SQLite's `user_version`) this code reads and writes.
const VERSION: i32 = 2;

/// Version 2. A note's `status` matters only for TODO notes; at most one
/// note is active. Ids aren't reused (`AUTOINCREMENT`), so an id in the
/// scrollback never means another note later.
const SCHEMA: &str = "
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

/// The columns [`Note::from_row`] reads. Times are shown in local time.
const COLUMNS: &str = "id, title, text, status, \
    strftime('%Y-%m-%d %H:%M', created, 'localtime'), \
    strftime('%Y-%m-%d %H:%M', done, 'localtime')";

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
    pub(crate) status: Status,
    /// When it was made, in local time (`YYYY-MM-DD HH:MM`).
    pub(crate) created: String,
    /// When it was done, like `created`.
    pub(crate) done: Option<String>,
}

impl Note {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        let status: String = row.get(3)?;
        Ok(Note {
            id: row.get(0)?,
            title: row.get(1)?,
            text: row.get(2)?,
            status: match status.as_str() {
                "active" => Status::Active,
                "done" => Status::Done,
                _ => Status::Open,
            },
            created: row.get(4)?,
            done: row.get(5)?,
        })
    }

    /// Whether it's a TODO note: its title or text starts with the word
    /// "todo", in any ASCII case.
    pub(crate) fn is_todo(&self) -> bool {
        self.title.as_deref().is_some_and(starts_with_todo) || starts_with_todo(&self.text)
    }
}

/// Whether `text` starts with the word "todo": "TODO: x", "todo x" and
/// "todo" do, "todos" doesn't.
fn starts_with_todo(text: &str) -> bool {
    let Some(rest) = text.get(4..) else {
        return false;
    };
    text[..4].eq_ignore_ascii_case("todo") && !rest.starts_with(char::is_alphanumeric)
}

/// A change to a note, made by [`Db::change`].
pub(crate) enum Change {
    /// Names it, or makes it unnamed again.
    Title(Option<String>),
    /// Replaces its title and text.
    Edit { title: Option<String>, text: String },
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
        migrate(&mut conn).map_err(|e| fail(&e))?;
        Ok(Db(conn))
    }

    /// Adds a note; returns its id.
    pub(crate) fn add(&self, title: Option<&str>, text: &str) -> rusqlite::Result<i64> {
        self.0.execute(
            "INSERT INTO notes (title, text) VALUES (?1, ?2)",
            params![title, text],
        )?;
        Ok(self.0.last_insert_rowid())
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

fn apply(tx: &Transaction<'_>, id: i64, change: Change) -> rusqlite::Result<()> {
    match change {
        Change::Title(title) => {
            tx.execute(
                "UPDATE notes SET title = ?2 WHERE id = ?1",
                params![id, title],
            )?;
        }
        Change::Edit { title, text } => {
            tx.execute(
                "UPDATE notes SET title = ?2, text = ?3 WHERE id = ?1",
                params![id, title, text],
            )?;
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
        0 => SCHEMA.to_owned(),
        1 => format!("{MIGRATE_1}{SCHEMA}{MIGRATE_1_COPY}"),
        _ => {
            return Err(format!(
                "the database has version {version}; this hob knows only up to {VERSION}"
            ));
        }
    };
    tx.execute_batch(&sql)
        .and_then(|()| tx.pragma_update(None, "user_version", VERSION))
        .and_then(|()| tx.commit())
        .map_err(|e| e.to_string())
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
        let a = db.add(None, "todo a").unwrap();
        let b = db.add(None, "todo b").unwrap();
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
    fn todo_is_a_word() {
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
    fn todo_notes_by_title_or_text() {
        let (mut db, _dir) = db();
        let a = db.add(None, "TODO: call").unwrap();
        let b = db.add(None, "plain").unwrap();
        let c = db.add(None, "not todo").unwrap();
        db.add(None, "todos").unwrap();
        set(&mut db, b, Change::Title(Some("ToDo later".into())));
        assert_eq!(ids(db.open_todos().unwrap()), [a, b]);
        assert_eq!(ids(db.unnamed().unwrap()), [c, c + 1]);
    }

    #[test]
    fn changes_see_what_is_stored() {
        let (mut db, _dir) = db();
        let a = db.add(Some("A"), "text").unwrap();
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
        db.add(None, "a").unwrap();
        let b = db.add(None, "b").unwrap();
        set(&mut db, b, Change::Delete);
        assert_eq!(db.add(None, "c").unwrap(), b + 1);
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
        assert_eq!(db.add(None, "new").unwrap(), 6);
        drop(db);
        // Opening it again changes nothing.
        let db = Db::open(&path).unwrap();
        assert_eq!(ids(db.all().unwrap()), [1, 2, 6]);
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
