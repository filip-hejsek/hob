//! [`Session`]: what a frontend saves to restore on the next start.

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use kettle_ui::FrozenIsland;
use serde::{Deserialize, Serialize};

/// The version of the session file format. Bump it on incompatible changes.
const VERSION: u32 = 1;

/// A frontend's state worth keeping across restarts: the prompt history and
/// the scrollback. Saved as JSON.
#[derive(Default, Serialize, Deserialize)]
pub struct Session {
    /// Entered command lines, oldest first.
    pub history: Vec<String>,
    /// The scrollback, oldest first.
    pub blocks: Vec<Block>,
}

/// A command line and its output.
#[derive(Serialize, Deserialize)]
pub struct Block {
    /// The command line as entered.
    pub line: String,
    /// What the command showed, in order.
    pub entries: Vec<Entry>,
}

/// Something a command showed.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Entry {
    /// A printed message.
    Text(String),
    /// An island, frozen (live islands are saved as they were).
    Island(FrozenIsland),
}

/// The file contents: the session, tagged with the format version.
#[derive(Serialize)]
struct FileRef<'a> {
    version: u32,
    #[serde(flatten)]
    session: &'a Session,
}

#[derive(Deserialize)]
struct Header {
    version: u32,
}

#[derive(Deserialize)]
struct File {
    #[serde(flatten)]
    session: Session,
}

impl Session {
    /// Reads a saved session. Returns `None` if there is no file.
    pub fn load(path: &Path) -> Result<Option<Session>, SessionError> {
        let json = match fs::read_to_string(path) {
            Ok(json) => json,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(SessionError::Io(e)),
        };
        // Check the version first, so a newer format isn't misreported as
        // a broken file.
        let Header { version } = serde_json::from_str(&json).map_err(SessionError::Format)?;
        if version != VERSION {
            return Err(SessionError::Version(version));
        }
        let File { session } = serde_json::from_str(&json).map_err(SessionError::Format)?;
        Ok(Some(session))
    }

    /// Saves the session, creating the directory if needed. The file is
    /// replaced atomically, so a crash can't leave it half-written.
    pub fn save(&self, path: &Path) -> Result<(), SessionError> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(SessionError::Io)?;
        }
        let file = FileRef {
            version: VERSION,
            session: self,
        };
        let json = serde_json::to_string_pretty(&file).map_err(SessionError::Format)?;
        let tmp = tmp_path(path);
        let write = || -> io::Result<()> {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(json.as_bytes())?;
            f.sync_all()?;
            fs::rename(&tmp, path)
        };
        write().map_err(|e| {
            let _ = fs::remove_file(&tmp);
            SessionError::Io(e)
        })
    }
}

/// Where [`Session::save`] writes before renaming: next to `path`, so the
/// rename stays on one file system.
fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(".tmp");
    path.with_file_name(name)
}

/// Why a session couldn't be loaded or saved.
#[derive(Debug)]
#[non_exhaustive]
pub enum SessionError {
    /// Reading or writing the file failed.
    Io(io::Error),
    /// The file isn't a valid session.
    Format(serde_json::Error),
    /// The file is from an unsupported version of the format.
    Version(u32),
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionError::Io(e) => write!(f, "{e}"),
            SessionError::Format(e) => write!(f, "invalid session file: {e}"),
            SessionError::Version(v) => write!(
                f,
                "session file has format version {v}, but only {VERSION} is supported"
            ),
        }
    }
}

impl std::error::Error for SessionError {}

#[cfg(test)]
mod tests {
    use kettle_ui::IslandBuilder;
    use kettle_ui::widget::*;

    use super::*;

    /// A fresh directory for one test.
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hob-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn session() -> Session {
        let mut b = IslandBuilder::new(text_field("name", ""));
        b.set_text("name", "Ada");
        Session {
            history: vec!["contact".into()],
            blocks: vec![Block {
                line: "contact".into(),
                entries: vec![
                    Entry::Island(b.freeze().unwrap()),
                    Entry::Text("done".into()),
                ],
            }],
        }
    }

    #[test]
    fn saves_and_loads() {
        let path = temp_dir("roundtrip").join("sub/session.json");
        session().save(&path).unwrap();
        let loaded = Session::load(&path).unwrap().unwrap();
        assert_eq!(loaded.history, ["contact"]);
        let [block] = &loaded.blocks[..] else {
            panic!("expected one block");
        };
        assert_eq!(block.line, "contact");
        assert!(matches!(&block.entries[0], Entry::Island(i) if i.fields().text("name") == "Ada"));
        assert!(matches!(&block.entries[1], Entry::Text(t) if t == "done"));
        assert!(!tmp_path(&path).exists());
    }

    #[test]
    fn missing_file_is_no_session() {
        let path = temp_dir("missing").join("session.json");
        assert!(Session::load(&path).unwrap().is_none());
    }

    #[test]
    fn other_versions_and_garbage_are_errors() {
        let dir = temp_dir("bad");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.json");
        fs::write(&path, r#"{"version": 99, "whatever": true}"#).unwrap();
        assert!(matches!(
            Session::load(&path),
            Err(SessionError::Version(99))
        ));
        fs::write(&path, "not json").unwrap();
        assert!(matches!(Session::load(&path), Err(SessionError::Format(_))));
    }
}
