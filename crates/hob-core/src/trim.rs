//! [`Limits`] on the size of a saved [`Session`](crate::Session), and the
//! [`Trim`] that keeps it within them.

use std::collections::HashSet;
use std::io;

use kettle_ui::{Image, ImageKey};

use crate::session::{Entry, Ephemeral, Item};

/// How big a saved session may get. Sizes are in bytes of the file, roughly:
/// the scrollback is measured without indentation.
///
/// The session is rewritten in full on every change, so the defaults keep
/// a save quick: a few MiB of text, and a bit more of images, which are
/// rarely as many.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The most history lines kept.
    pub history: usize,
    /// The size of the scrollback, without image data. When it's exceeded,
    /// the oldest items are dropped.
    pub scrollback: usize,
    /// The size of the image data. When it's exceeded, images of the oldest
    /// items lose their data, so only their alt text is shown.
    pub images: usize,
}

const MIB: usize = 1024 * 1024;

impl Default for Limits {
    fn default() -> Self {
        Limits {
            history: 10_000,
            scrollback: 4 * MIB,
            images: 16 * MIB,
        }
    }
}

impl Limits {
    /// The default limits, with the sizes overridden by the environment
    /// variables `HOB_SESSION_SIZE` (the scrollback) and
    /// `HOB_SESSION_IMAGES`, if set: a number of bytes, optionally followed
    /// by `K`, `M` or `G` (KiB, MiB, GiB). Invalid values are ignored, with
    /// a logged warning.
    pub fn from_env() -> Limits {
        let mut limits = Limits::default();
        for (var, limit) in [
            ("HOB_SESSION_SIZE", &mut limits.scrollback),
            ("HOB_SESSION_IMAGES", &mut limits.images),
        ] {
            let Ok(value) = std::env::var(var) else {
                continue;
            };
            match parse_size(&value) {
                Some(size) => *limit = size,
                None => log::warn!("ignoring {var}: not a size: {value:?}"),
            }
        }
        limits
    }
}

/// A size such as `512`, `64K` or `4M`.
fn parse_size(s: &str) -> Option<usize> {
    let s = s.trim();
    let (number, unit) = match s.char_indices().last()? {
        (i, 'k' | 'K') => (&s[..i], 1024),
        (i, 'm' | 'M') => (&s[..i], MIB),
        (i, 'g' | 'G') => (&s[..i], 1024 * MIB),
        _ => (s, 1),
    };
    number.parse::<usize>().ok()?.checked_mul(unit)
}

/// What to trim from a session's scrollback to keep it within [`Limits`],
/// as worked out by [`Trim::new`]. The newest item is always kept whole.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Trim {
    /// Items before this index are dropped.
    pub keep_from: usize,
    /// Items before this index lose their images' data (see
    /// [`strip_image`]). Not less than `keep_from`.
    pub images_from: usize,
}

impl Trim {
    /// Works out what to trim from `items` (oldest first, as saved) to
    /// keep them within `limits`: counting from the newest, the items up to
    /// the one that exceeds a limit are kept. Ephemeral items and entries
    /// count as nothing, as do images that aren't saved with their data
    /// (linked, ephemeral or without data), and an image is counted once,
    /// at its newest use.
    pub fn new(items: &[Item], limits: &Limits) -> Trim {
        let mut trim = Trim::default();
        let (mut size, mut images) = (0, 0);
        let mut seen = HashSet::new();
        // The newest item is skipped: it's always kept, so it can't be
        // where a limit is exceeded, but it counts.
        for (i, item) in items.iter().enumerate().rev() {
            size += saved_size(item);
            if size > limits.scrollback && i + 1 < items.len() {
                trim.keep_from = i + 1;
                trim.images_from = trim.images_from.max(i + 1);
                break;
            }
            if trim.images_from == 0 {
                images += image_size(item, &mut seen);
                if images > limits.images && i + 1 < items.len() {
                    trim.images_from = i + 1;
                }
            }
        }
        trim
    }

    /// Whether nothing is trimmed.
    pub fn is_empty(&self) -> bool {
        self.images_from == 0
    }
}

/// `image` as kept in an item that lost its images: without data, unless
/// it's linked, since then it isn't saved anyway.
pub fn strip_image(image: &Image) -> Image {
    if image.is_linked() {
        image.clone()
    } else {
        image.without_data()
    }
}

/// The size of an item as saved, without indentation and image data.
fn saved_size(item: &Item) -> usize {
    if item.is_ephemeral() {
        return 0;
    }
    let mut counter = Counter(0);
    // Serializing items can't fail: they're all valid JSON values.
    serde_json::to_writer(&mut counter, item).expect("item can't be serialized");
    counter.0
}

/// The size of the data of the images in `item` that are saved with it, in
/// base64, except for those in `seen`, which are added to it.
fn image_size(item: &Item, seen: &mut HashSet<ImageKey>) -> usize {
    let Item::Block(block) = item else {
        return 0;
    };
    (block.entries.iter())
        .filter_map(|entry| match entry {
            Entry::Island(island) => Some(island.images()),
            _ => None,
        })
        .flatten()
        .filter(|image| !image.is_ephemeral() && !image.is_linked())
        .filter_map(|image| Some((image.key(), image.data()?)))
        .filter(|(key, _)| seen.insert(*key))
        .map(|(_, data)| data.bytes().len().div_ceil(3) * 4)
        .sum()
}

/// Counts the bytes written to it.
struct Counter(usize);

impl io::Write for Counter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0 += buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use kettle_ui::IslandBuilder;
    use kettle_ui::widget::image as image_widget;

    use super::*;
    use crate::session::Block;
    use crate::{Level, LogRecord};

    fn block(entries: Vec<Entry>) -> Item {
        Item::Block(Block {
            line: "cmd".into(),
            entries,
            interrupted: false,
        })
    }

    fn text(n: usize) -> Item {
        block(vec![Entry::Text("x".repeat(n))])
    }

    fn picture(shade: u8) -> Image {
        Image::from_rgba(4, 4, &[shade, 0, 0, 255].repeat(16), "picture").unwrap()
    }

    fn with_image(image: Image) -> Item {
        let mut b = IslandBuilder::new(image_widget("x"));
        b.set_state("x", image);
        block(vec![Entry::Island(b.freeze().unwrap())])
    }

    fn base64_size(image: &Image) -> usize {
        image.data().unwrap().bytes().len().div_ceil(3) * 4
    }

    /// Limits that let only the scrollback size matter.
    fn scrollback(size: usize) -> Limits {
        Limits {
            scrollback: size,
            images: usize::MAX,
            ..Limits::default()
        }
    }

    #[test]
    fn sizes_are_parsed() {
        assert_eq!(parse_size("512"), Some(512));
        assert_eq!(parse_size("64K"), Some(64 * 1024));
        assert_eq!(parse_size(" 4m "), Some(4 * MIB));
        assert_eq!(parse_size("1G"), Some(1024 * MIB));
        assert_eq!(parse_size("M"), None);
        assert_eq!(parse_size("-1"), None);
        assert_eq!(parse_size("1.5M"), None);
    }

    #[test]
    fn oldest_items_are_dropped() {
        let items: Vec<_> = (0..5).map(|_| text(100)).collect();
        let size = saved_size(&items[0]);
        assert!(size > 100);
        let trim = |limit| Trim::new(&items, &scrollback(limit));
        assert!(trim(5 * size).is_empty());
        assert_eq!(trim(5 * size - 1).keep_from, 1);
        assert_eq!(trim(2 * size + size / 2).keep_from, 3);
        assert_eq!(trim(2 * size + size / 2).images_from, 3);
        // The newest item is kept even if it's too big.
        assert_eq!(trim(1).keep_from, 4);
        assert_eq!(Trim::new(&items[..1], &scrollback(1)), Trim::default());
        assert_eq!(Trim::new(&[], &scrollback(0)), Trim::default());
    }

    #[test]
    fn ephemeral_records_count_as_nothing() {
        let big = "x".repeat(1000);
        let record = || LogRecord::new(Level::Error, "t", &big).ephemeral();
        let items = [
            text(10),
            Item::Notice(record()),
            block(vec![Entry::Log(record())]),
            text(10),
        ];
        assert!(Trim::new(&items, &scrollback(500)).is_empty());
    }

    #[test]
    fn old_images_lose_their_data() {
        let (a, b, c) = (picture(10), picture(20), picture(30));
        let items = [
            with_image(a.clone()),
            with_image(b.clone()),
            // Counted once, as the newest use.
            with_image(b.clone()),
            // Not saved with their data.
            with_image(picture(40).ephemeral()),
            with_image(picture(50).without_data()),
            with_image(c.clone()),
        ];
        let limits = |images| Limits {
            images,
            ..Limits::default()
        };
        let both = base64_size(&b) + base64_size(&c);
        let all = both + base64_size(&a);
        assert!(Trim::new(&items, &limits(all)).is_empty());
        let trim = Trim::new(&items, &limits(all - 1));
        assert_eq!(
            trim,
            Trim {
                keep_from: 0,
                images_from: 1
            }
        );
        let trim = Trim::new(&items, &limits(both - 1));
        assert_eq!(
            trim,
            Trim {
                keep_from: 0,
                images_from: 3
            }
        );
        // The newest item keeps its images.
        assert_eq!(Trim::new(&items, &limits(0)).images_from, 5);
    }

    #[test]
    fn stripping_keeps_linked_images() {
        let dir = std::env::temp_dir().join(format!("hob-test-{}-strip", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("picture.png");
        std::fs::write(&path, picture(10).data().unwrap().bytes()).unwrap();
        let linked = Image::from_file(&path, "linked").unwrap().linked();
        assert!(strip_image(&linked).data().is_some());
        assert!(strip_image(&picture(10)).data().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
