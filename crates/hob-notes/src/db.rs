//! The notes database: one SQLite file, opened by each command that needs it.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, Row, params};

/// The schema version (SQLite's `user_version`) this code reads and writes.
const VERSION: i32 = 1;

/// Version 1. A note's `status` matters only for TODO notes; at most one
/// note is active.
const SCHEMA: &str = "
CREATE TABLE notes (
    id INTEGER PRIMARY KEY,
    title TEXT,
    text TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'active', 'done')),
    created TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    done TEXT
);
CREATE UNIQUE INDEX notes_one_active ON notes (status) WHERE status = 'active';
";

/// The columns [`Note::from_row`] reads.
const COLUMNS: &str = "id, title, text";

/// A stored note.
#[derive(Clone, Debug)]
pub(crate) struct Note {
    pub(crate) id: i64,
    /// `None` until it's named.
    pub(crate) title: Option<String>,
    pub(crate) text: String,
}

impl Note {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Note {
            id: row.get(0)?,
            title: row.get(1)?,
            text: row.get(2)?,
        })
    }
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

    /// Adds an unnamed note; returns its id.
    pub(crate) fn add(&self, text: &str) -> rusqlite::Result<i64> {
        self.0
            .execute("INSERT INTO notes (text) VALUES (?1)", [text])?;
        Ok(self.0.last_insert_rowid())
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

    /// Names (or renames) a note.
    pub(crate) fn set_title(&self, id: i64, title: &str) -> rusqlite::Result<()> {
        (self.0).execute(
            "UPDATE notes SET title = ?2 WHERE id = ?1",
            params![id, title],
        )?;
        Ok(())
    }

    /// Makes a note active; the one active before goes back to open.
    pub(crate) fn activate(&mut self, id: i64) -> rusqlite::Result<()> {
        let tx = self.0.transaction()?;
        tx.execute(
            "UPDATE notes SET status = 'open' WHERE status = 'active'",
            [],
        )?;
        tx.execute(
            "UPDATE notes SET status = 'active', done = NULL WHERE id = ?1",
            [id],
        )?;
        tx.commit()
    }

    /// Marks a note done.
    pub(crate) fn finish(&self, id: i64) -> rusqlite::Result<()> {
        self.0.execute(
            "UPDATE notes SET status = 'done', \
             done = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?1",
            [id],
        )?;
        Ok(())
    }

    /// Up to `limit` unnamed notes, oldest first.
    pub(crate) fn unnamed(&self, limit: usize) -> rusqlite::Result<Vec<Note>> {
        self.many(
            &format!("SELECT {COLUMNS} FROM notes WHERE title IS NULL ORDER BY id LIMIT ?1"),
            limit,
        )
    }

    /// Up to `limit` open TODO notes, oldest first: those whose title or
    /// text starts with "todo", in any case. Both are trimmed when saved,
    /// and `LIKE` ignores ASCII case.
    pub(crate) fn open_todos(&self, limit: usize) -> rusqlite::Result<Vec<Note>> {
        self.many(
            &format!(
                "SELECT {COLUMNS} FROM notes WHERE status = 'open' \
                 AND (title LIKE 'todo%' OR text LIKE 'todo%') ORDER BY id LIMIT ?1"
            ),
            limit,
        )
    }

    fn one(&self, sql: &str) -> rusqlite::Result<Option<Note>> {
        self.0.query_row(sql, [], Note::from_row).optional()
    }

    fn many(&self, sql: &str, limit: usize) -> rusqlite::Result<Vec<Note>> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut statement = self.0.prepare(sql)?;
        let notes = statement.query_map([limit], Note::from_row)?;
        notes.collect()
    }
}

/// Creates the tables in a new database; refuses one from a newer version.
fn migrate(conn: &mut Connection) -> Result<(), String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let version: i32 = tx
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|e| e.to_string())?;
    match version {
        VERSION => Ok(()),
        0 => tx
            .execute_batch(SCHEMA)
            .and_then(|()| tx.pragma_update(None, "user_version", VERSION))
            .and_then(|()| tx.commit())
            .map_err(|e| e.to_string()),
        _ => Err(format!(
            "the database has version {version}; this hob knows only up to {VERSION}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> (Db, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("sub/notes.sqlite")).unwrap();
        (db, dir)
    }

    #[test]
    fn one_note_is_active() {
        let (mut db, _dir) = db();
        let a = db.add("todo a").unwrap();
        let b = db.add("todo b").unwrap();
        let open =
            |db: &Db| -> Vec<i64> { db.open_todos(10).unwrap().iter().map(|n| n.id).collect() };
        db.activate(a).unwrap();
        db.activate(b).unwrap();
        assert_eq!(db.active().unwrap().unwrap().id, b);
        assert_eq!(open(&db), [a]);
        db.finish(b).unwrap();
        assert!(db.active().unwrap().is_none());
        assert_eq!(open(&db), [a]);
    }

    #[test]
    fn todo_notes_by_title_or_text() {
        let (db, _dir) = db();
        let a = db.add("TODO: call").unwrap();
        let b = db.add("plain").unwrap();
        db.add("not todo").unwrap();
        db.set_title(b, "ToDo later").unwrap();
        let ids: Vec<_> = db.open_todos(10).unwrap().iter().map(|n| n.id).collect();
        assert_eq!(ids, [a, b]);
        assert_eq!(db.open_todos(1).unwrap().len(), 1);
    }

    #[test]
    fn refuses_newer_versions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
        drop(conn);
        let error = Db::open(&path).err().unwrap();
        assert!(error.contains("version 99"), "{error}");
    }
}
