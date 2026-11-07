//! [`Session`]: what a frontend saves to restore on the next start.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use kettle_ui::{FrozenIsland, Image, ImageData, ImageKey};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::LogRecord;

/// The version of the session file format. Bump it on incompatible changes.
/// Version 1 had no log records, and `blocks` instead of `scrollback`;
/// version 2 had no images.
const VERSION: u32 = 3;

/// A frontend's state worth keeping across restarts: the prompt history and
/// the scrollback. Saved as JSON, without ephemeral log records.
///
/// Images are saved once each, in a table by [`ImageKey`] at the end of the
/// file, and islands refer to them by key. Linked images
/// ([`Image::linked`]) are saved as their path and key: they're read from
/// the file again when loaded, if it hasn't changed. Ephemeral images
/// ([`Image::ephemeral`]) aren't saved, nor their paths. Images restored
/// without data show only their alt text.
#[derive(Default, Serialize, Deserialize)]
pub struct Session {
    /// Entered command lines, oldest first.
    pub history: Vec<String>,
    /// The scrollback, oldest first.
    #[serde(serialize_with = "skip_ephemeral")]
    pub scrollback: Vec<Item>,
}

/// Something in the scrollback.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Item {
    /// A command line and its output.
    Block(Block),
    /// A log record that isn't from a command, e.g. an error of the
    /// frontend itself.
    Notice(LogRecord),
}

/// A command line and its output.
#[derive(Serialize, Deserialize)]
pub struct Block {
    /// The command line as entered.
    pub line: String,
    /// What the command showed, in order.
    #[serde(serialize_with = "skip_ephemeral")]
    pub entries: Vec<Entry>,
    /// Whether the command was still running when the session was saved, so
    /// it was interrupted when the frontend exited.
    #[serde(default)]
    pub interrupted: bool,
}

/// Something a command showed.
pub enum Entry {
    /// A printed message.
    Text(String),
    /// A log record, e.g. from [`Context::error`](crate::Context::error).
    Log(LogRecord),
    /// An island, frozen (live islands are saved as they were).
    Island(FrozenIsland),
    /// A saved island that couldn't be read, e.g. one saved by a newer
    /// version. It's saved back unchanged, so it isn't lost.
    Unreadable {
        /// Why it couldn't be read.
        error: String,
        /// The island as saved.
        raw: serde_json::Value,
    },
}

/// How an [`Entry`] is saved.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum EntryRepr<T, L, I> {
    Text(T),
    Log(L),
    Island(I),
}

/// An [`EntryRepr`] that borrows from the entry, for saving.
type EntryRef<'a, I> = EntryRepr<&'a str, &'a LogRecord, I>;

impl Serialize for Entry {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Entry::Text(text) => EntryRef::<()>::Text(text).serialize(serializer),
            Entry::Log(record) => EntryRef::<()>::Log(record).serialize(serializer),
            Entry::Island(island) => {
                EntryRef::Island(island.map_images(saved_image)).serialize(serializer)
            }
            Entry::Unreadable { raw, .. } => EntryRef::Island(raw).serialize(serializer),
        }
    }
}

/// An image as saved in an island: without data, which is saved in the
/// table (or not saved, if it's linked), and without the path if it's
/// ephemeral.
fn saved_image(image: &Image) -> Image {
    let image = image.without_data();
    if image.is_ephemeral() {
        image.without_path()
    } else {
        image
    }
}

/// Things that may be left out when saving.
trait Ephemeral {
    fn is_ephemeral(&self) -> bool;
}

impl Ephemeral for Item {
    fn is_ephemeral(&self) -> bool {
        matches!(self, Item::Notice(record) if record.ephemeral)
    }
}

impl Ephemeral for Entry {
    fn is_ephemeral(&self) -> bool {
        matches!(self, Entry::Log(record) if record.ephemeral)
    }
}

/// Serializes the items that aren't ephemeral.
fn skip_ephemeral<S: Serializer, T: Serialize + Ephemeral>(
    items: &[T],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_seq(items.iter().filter(|item| !item.is_ephemeral()))
}

impl<'de> Deserialize<'de> for Entry {
    /// An island that can't be read becomes [`Entry::Unreadable`] instead of
    /// failing the whole session.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(
            match EntryRepr::<String, LogRecord, serde_json::Value>::deserialize(deserializer)? {
                EntryRepr::Text(text) => Entry::Text(text),
                EntryRepr::Log(record) => Entry::Log(record),
                EntryRepr::Island(raw) => match FrozenIsland::deserialize(&raw) {
                    Ok(island) => Entry::Island(island),
                    Err(e) => Entry::Unreadable {
                        error: e.to_string(),
                        raw,
                    },
                },
            },
        )
    }
}

/// The file contents: the session, tagged with the format version, and the
/// images.
#[derive(Serialize)]
struct FileRef<'a> {
    version: u32,
    #[serde(flatten)]
    session: &'a Session,
    images: BTreeMap<ImageKey, ImageData>,
}

#[derive(Deserialize)]
struct Header {
    version: u32,
}

#[derive(Deserialize)]
struct File {
    #[serde(flatten)]
    session: Session,
    #[serde(default)]
    images: BTreeMap<ImageKey, ImageData>,
}

/// A version 1 file.
#[derive(Deserialize)]
struct FileV1 {
    history: Vec<String>,
    blocks: Vec<Block>,
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
        let session = match version {
            1 => {
                let FileV1 { history, blocks } =
                    serde_json::from_str(&json).map_err(SessionError::Format)?;
                Session {
                    history,
                    scrollback: blocks.into_iter().map(Item::Block).collect(),
                }
            }
            2 | VERSION => {
                let File {
                    mut session,
                    images,
                } = serde_json::from_str(&json).map_err(SessionError::Format)?;
                session.attach_images(&images);
                session
            }
            _ => return Err(SessionError::Version(version)),
        };
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
            images: self.image_table(),
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

impl Session {
    /// The entries of all blocks.
    fn entries(&self) -> impl Iterator<Item = &Entry> {
        (self.scrollback.iter())
            .filter_map(|item| match item {
                Item::Block(block) => Some(block.entries.iter()),
                Item::Notice(_) => None,
            })
            .flatten()
    }

    /// The data of the islands' images that are saved with their data.
    fn image_table(&self) -> BTreeMap<ImageKey, ImageData> {
        (self.entries())
            .filter_map(|entry| match entry {
                Entry::Island(island) => Some(island.images()),
                _ => None,
            })
            .flatten()
            .filter(|image| !image.is_ephemeral() && !image.is_linked())
            .filter_map(|image| Some((image.key(), image.data()?.clone())))
            .collect()
    }

    /// Gives the loaded islands' images their data from the table, or for
    /// linked images, from their files if unchanged. Images of unreadable
    /// islands get it inline from the table, so it's saved with them.
    fn attach_images(&mut self, images: &BTreeMap<ImageKey, ImageData>) {
        let entries = (self.scrollback.iter_mut())
            .filter_map(|item| match item {
                Item::Block(block) => Some(block.entries.iter_mut()),
                Item::Notice(_) => None,
            })
            .flatten();
        for entry in entries {
            match entry {
                Entry::Island(island) => {
                    *island = island.map_images(|image| match images.get(&image.key()) {
                        Some(data) => image.with_data(data.clone()),
                        None if image.is_linked() => {
                            image.reread().unwrap_or_else(|_| image.clone())
                        }
                        None => image.clone(),
                    });
                }
                Entry::Unreadable { raw, .. } => attach_raw_images(raw, images),
                Entry::Text(_) | Entry::Log(_) => {}
            }
        }
    }
}

/// Puts image data into the images of an island that can't be read, as far
/// as they can be found: objects in its named state with a known `key`.
fn attach_raw_images(raw: &mut serde_json::Value, images: &BTreeMap<ImageKey, ImageData>) {
    let Some(state) = raw.get_mut("state").and_then(|s| s.as_object_mut()) else {
        return;
    };
    for value in state.values_mut() {
        let Some(image) = value.as_object_mut() else {
            continue;
        };
        let key = image.get("key").and_then(|k| k.as_str()?.parse().ok());
        let data = key.and_then(|key: ImageKey| serde_json::to_value(images.get(&key)?).ok());
        if let Some(serde_json::Value::Object(data)) = data {
            for (k, v) in data {
                image.entry(k).or_insert(v);
            }
        }
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
                "session file has format version {v}, but only up to {VERSION} is supported"
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
    use crate::Level;

    /// The only block of a session's scrollback.
    fn only_block(session: &Session) -> &Block {
        let [Item::Block(block)] = &session.scrollback[..] else {
            panic!("expected one block");
        };
        block
    }

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
            scrollback: vec![Item::Block(Block {
                line: "contact".into(),
                entries: vec![
                    Entry::Island(b.freeze().unwrap()),
                    Entry::Text("done".into()),
                ],
                interrupted: true,
            })],
        }
    }

    #[test]
    fn saves_and_loads() {
        let path = temp_dir("roundtrip").join("sub/session.json");
        session().save(&path).unwrap();
        let loaded = Session::load(&path).unwrap().unwrap();
        assert_eq!(loaded.history, ["contact"]);
        let block = only_block(&loaded);
        assert_eq!(block.line, "contact");
        assert!(matches!(&block.entries[0], Entry::Island(i) if i.fields().text("name") == "Ada"));
        assert!(matches!(&block.entries[1], Entry::Text(t) if t == "done"));
        assert!(block.interrupted);
        assert!(!tmp_path(&path).exists());
    }

    #[test]
    fn unreadable_island_is_kept_as_is() {
        let path = temp_dir("unreadable").join("session.json");
        let mut json = serde_json::to_value(FileRef {
            version: VERSION,
            session: &session(),
            images: BTreeMap::new(),
        })
        .unwrap();
        let island = &mut json["scrollback"][0]["block"]["entries"][0]["island"];
        island["fields"]["name"] = true.into();
        let broken = island.clone();
        // Older files have no "interrupted".
        json["scrollback"][0]["block"]
            .as_object_mut()
            .unwrap()
            .remove("interrupted");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, json.to_string()).unwrap();

        let loaded = Session::load(&path).unwrap().unwrap();
        let block = only_block(&loaded);
        assert!(!block.interrupted);
        let Entry::Unreadable { error, raw } = &block.entries[0] else {
            panic!("expected an unreadable island");
        };
        assert!(error.contains("not a checkbox"), "{error}");
        assert_eq!(raw, &broken);
        assert!(matches!(&block.entries[1], Entry::Text(t) if t == "done"));

        loaded.save(&path).unwrap();
        let saved: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            saved["scrollback"][0]["block"]["entries"][0]["island"],
            broken
        );
    }

    #[test]
    fn ephemeral_records_are_not_saved() {
        let path = temp_dir("ephemeral").join("session.json");
        let mut s = session();
        let Item::Block(block) = &mut s.scrollback[0] else {
            unreachable!()
        };
        block.entries = vec![
            Entry::Log(LogRecord::new(Level::Error, "contact", "kept")),
            Entry::Log(LogRecord::new(Level::Debug, "contact", "dropped")),
        ];
        s.scrollback.extend([
            Item::Notice(LogRecord::new(Level::Warn, "hob_gui", "kept too")),
            Item::Notice(LogRecord::new(Level::Error, "hob_gui", "dropped").ephemeral()),
        ]);
        s.save(&path).unwrap();

        let loaded = Session::load(&path).unwrap().unwrap();
        let [Item::Block(block), Item::Notice(notice)] = &loaded.scrollback[..] else {
            panic!("expected a block and a notice");
        };
        let [Entry::Log(record)] = &block.entries[..] else {
            panic!("expected one log record");
        };
        assert_eq!((record.level, &*record.message), (Level::Error, "kept"));
        assert_eq!(notice.message, "kept too");
    }

    fn red(shade: u8, alt: &str) -> Image {
        Image::from_rgba(2, 2, &[shade, 0, 0, 255].repeat(4), alt).unwrap()
    }

    fn image_island(images: &[(&str, Image)]) -> Entry {
        let names = images.iter().map(|(name, _)| image(name));
        let mut b = IslandBuilder::new(column(names));
        for (name, img) in images {
            b.set_state(name, img.clone());
        }
        Entry::Island(b.freeze().unwrap())
    }

    /// Image named state of an island entry.
    fn image_state(entry: &Entry, name: &str) -> Image {
        let Entry::Island(island) = entry else {
            panic!("expected an island");
        };
        match island.state(name) {
            Some(kettle_ui::Value::Image(image)) => image.clone(),
            other => panic!("not an image: {other:?}"),
        }
    }

    #[test]
    fn images_are_saved_once() {
        let path = temp_dir("images").join("session.json");
        let (a, b) = (red(10, "a"), red(20, "b"));
        let secret = red(30, "secret").ephemeral();
        let mut s = session();
        let Item::Block(block) = &mut s.scrollback[0] else {
            unreachable!()
        };
        block.entries = vec![
            image_island(&[("x", a.clone()), ("y", b.clone()), ("z", secret)]),
            // The same image again, with another alt text.
            image_island(&[("x", red(10, "a again"))]),
        ];
        s.save(&path).unwrap();

        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let table = json["images"].as_object().unwrap();
        assert_eq!(table.len(), 2);
        assert!(table.contains_key(&a.key().to_string()));
        let island = &json["scrollback"][0]["block"]["entries"][0]["island"];
        assert_eq!(island["state"]["x"]["key"], a.key().to_string());
        assert!(island["state"]["x"].get("data").is_none());

        let loaded = Session::load(&path).unwrap().unwrap();
        let block = only_block(&loaded);
        assert_eq!(image_state(&block.entries[0], "x").data(), a.data());
        assert_eq!(image_state(&block.entries[0], "y").data(), b.data());
        let secret = image_state(&block.entries[0], "z");
        assert_eq!(secret.alt(), "secret");
        assert!(secret.data().is_none());
        let again = image_state(&block.entries[1], "x");
        assert_eq!((again.alt(), again.data()), ("a again", a.data()));
    }

    #[test]
    fn linked_images_are_read_again_if_unchanged() {
        let dir = temp_dir("linked");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.json");
        let file = |name: &str, shade: u8| {
            let file = dir.join(name);
            fs::write(&file, red(shade, "").data().unwrap().bytes()).unwrap();
            Image::from_file(&file, name).unwrap().linked()
        };
        let (kept, changed) = (file("kept.png", 10), file("changed.png", 20));
        let mut s = session();
        let Item::Block(block) = &mut s.scrollback[0] else {
            unreachable!()
        };
        block.entries = vec![image_island(&[("x", kept.clone()), ("y", changed)])];
        s.save(&path).unwrap();

        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(json["images"].as_object().unwrap().is_empty());
        let x = &json["scrollback"][0]["block"]["entries"][0]["island"]["state"]["x"];
        assert_eq!(x["path"], dir.join("kept.png").to_str().unwrap());
        assert_eq!(x["linked"], true);

        file("changed.png", 30);
        let loaded = Session::load(&path).unwrap().unwrap();
        let block = only_block(&loaded);
        let x = image_state(&block.entries[0], "x");
        assert!(x.is_linked());
        assert_eq!(
            x.data().unwrap().bytes(),
            fs::read(kept.path().unwrap()).unwrap()
        );
        let y = image_state(&block.entries[0], "y");
        assert!(y.is_linked() && y.path().is_some() && y.data().is_none());
    }

    #[test]
    fn unreadable_islands_keep_their_images() {
        let path = temp_dir("unreadable-images").join("session.json");
        let a = red(10, "a");
        let mut s = session();
        let Item::Block(block) = &mut s.scrollback[0] else {
            unreachable!()
        };
        block.entries = vec![image_island(&[("x", a.clone())])];
        s.save(&path).unwrap();
        let mut json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        json["scrollback"][0]["block"]["entries"][0]["island"]["root"] = "unknown".into();
        fs::write(&path, json.to_string()).unwrap();

        let loaded = Session::load(&path).unwrap().unwrap();
        let block = only_block(&loaded);
        let Entry::Unreadable { raw, .. } = &block.entries[0] else {
            panic!("expected an unreadable island");
        };
        let data = serde_json::to_value(a.data().unwrap()).unwrap();
        assert_eq!(raw["state"]["x"]["data"], data["data"]);
    }

    #[test]
    fn version_1_is_converted() {
        let path = temp_dir("v1").join("session.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let v1 = r#"{"version": 1, "history": ["echo hi"],
            "blocks": [{"line": "echo hi", "entries": [{"text": "hi"}]}]}"#;
        fs::write(&path, v1).unwrap();
        let loaded = Session::load(&path).unwrap().unwrap();
        assert_eq!(loaded.history, ["echo hi"]);
        let block = only_block(&loaded);
        assert!(matches!(&block.entries[..], [Entry::Text(t)] if t == "hi"));
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
