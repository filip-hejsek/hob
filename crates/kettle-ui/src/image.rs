//! [`Image`]: named state shown by [`widget::image`](crate::widget::image).

mod codec;

use std::any::Any;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, OnceLock};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// An image, as [named state](crate::Value::Image) for
/// [`widget::image`](crate::widget::image). Cheap to clone.
///
/// It's made from 8-bit RGBA pixels ([`Image::from_rgba`]), an image file's
/// contents ([`Image::from_bytes`]: PNG, JPEG, GIF, WebP or JPEG XL), or a
/// file ([`Image::from_file`]), and kept compressed (see [`ImageData`]).
/// Compressing can take a while for big images, so make images on a
/// command's thread or in a derived computation, not in a UI thread.
///
/// Every image has an alt text, shown in its place when the image isn't
/// available (e.g. an [ephemeral](Image::ephemeral) image in a restored
/// session).
///
/// A frontend that saves images (e.g. in a session) saves a copy of the
/// data by default; an image can instead be [linked](Image::linked) to its
/// file, or [ephemeral](Image::ephemeral).
///
/// Serialized as an object with the [`key`](Image::key), the alt text,
/// the [`ImageData`] fields if the data is there, the path if the image
/// is from a file, and `linked: true` if it's linked.
#[derive(Clone)]
pub struct Image(Arc<Inner>);

/// How a frontend saves an image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Saving {
    /// With a copy of its data.
    Copy,
    /// See [`Image::linked`].
    Link,
    /// See [`Image::ephemeral`].
    Ephemeral,
}

struct Inner {
    key: ImageKey,
    alt: String,
    data: Option<ImageData>,
    path: Option<PathBuf>,
    saving: Saving,
    /// See [`Image::backend_cache`].
    cache: OnceLock<Box<dyn Any + Send + Sync>>,
}

impl Image {
    /// An image of 8-bit RGBA pixels (not premultiplied), rows top to bottom.
    /// It's compressed losslessly.
    pub fn from_rgba(
        width: u32,
        height: u32,
        pixels: &[u8],
        alt: impl Into<String>,
    ) -> Result<Image, ImageError> {
        let key = ImageKey::of_rgba(width, height, pixels);
        let data = codec::compress_rgba(width, height, pixels)?;
        Ok(Image::new(key, alt.into(), Some(data), None))
    }

    /// An image from an image file's contents: PNG, JPEG, GIF, WebP or
    /// JPEG XL. Lossless images and JPEGs are recompressed (losslessly) as
    /// JPEG XL if that's smaller.
    pub fn from_bytes(
        bytes: impl Into<Vec<u8>>,
        alt: impl Into<String>,
    ) -> Result<Image, ImageError> {
        let bytes = bytes.into();
        let key = ImageKey::of_encoded(&bytes);
        let data = codec::compress(bytes)?;
        Ok(Image::new(key, alt.into(), Some(data), None))
    }

    /// An image from a file, like [`Image::from_bytes`] with its contents.
    /// The image remembers the (absolute) path, which a frontend may pass on
    /// to its client.
    pub fn from_file(path: impl AsRef<Path>, alt: impl Into<String>) -> Result<Image, ImageError> {
        let path = std::path::absolute(path.as_ref()).map_err(ImageError::Io)?;
        let bytes = std::fs::read(&path).map_err(ImageError::Io)?;
        let key = ImageKey::of_encoded(&bytes);
        let data = codec::compress(bytes)?;
        Ok(Image::new(key, alt.into(), Some(data), Some(path)))
    }

    fn new(key: ImageKey, alt: String, data: Option<ImageData>, path: Option<PathBuf>) -> Image {
        Image(Arc::new(Inner {
            key,
            alt,
            data,
            path,
            saving: Saving::Copy,
            cache: OnceLock::new(),
        }))
    }

    /// A copy with some fields changed (and an empty backend cache).
    fn with(&self, change: impl FnOnce(&mut Inner)) -> Image {
        let mut inner = Inner {
            key: self.0.key,
            alt: self.0.alt.clone(),
            data: self.0.data.clone(),
            path: self.0.path.clone(),
            saving: self.0.saving,
            cache: OnceLock::new(),
        };
        change(&mut inner);
        Image(Arc::new(inner))
    }

    /// Marks the image as not to be saved: a frontend leaves it out of what
    /// it saves to disk (e.g. a session), so after a restart only the alt
    /// text is shown. It's still shown, and sent to clients, as usual.
    /// Replaces [`Image::linked`].
    pub fn ephemeral(self) -> Image {
        self.with(|i| i.saving = Saving::Ephemeral)
    }

    /// Whether the image is [ephemeral](Image::ephemeral).
    pub fn is_ephemeral(&self) -> bool {
        self.0.saving == Saving::Ephemeral
    }

    /// Marks the image as linked to its file: a frontend saves only its
    /// path and key, and reads the file again when restoring it (see
    /// [`Image::reread`]), so it's shown only if the file hasn't changed.
    /// For an image that isn't from a file, that's like
    /// [ephemeral](Image::ephemeral). Replaces [`Image::ephemeral`].
    pub fn linked(self) -> Image {
        self.with(|i| i.saving = Saving::Link)
    }

    /// Whether the image is [linked](Image::linked).
    pub fn is_linked(&self) -> bool {
        self.0.saving == Saving::Link
    }

    /// A copy with the data read again from the image's file, if the file
    /// still has the contents the image was made from (the same
    /// [`key`](Image::key)). The contents are kept as they are, not
    /// recompressed, so this is quick.
    pub fn reread(&self) -> Result<Image, ImageError> {
        let path = self.path().ok_or(ImageError::NoFile)?;
        let bytes = std::fs::read(path).map_err(ImageError::Io)?;
        if ImageKey::of_encoded(&bytes) != self.key() {
            return Err(ImageError::Changed);
        }
        let data = codec::take(bytes)?;
        Ok(self.with_data(data))
    }

    /// Identifies the data the image was made from, before compression.
    pub fn key(&self) -> ImageKey {
        self.0.key
    }

    /// The alt text.
    pub fn alt(&self) -> &str {
        &self.0.alt
    }

    /// The compressed image, unless it's missing (see
    /// [`Image::without_data`]).
    pub fn data(&self) -> Option<&ImageData> {
        self.0.data.as_ref()
    }

    /// The file the image was read from, if any.
    pub fn path(&self) -> Option<&Path> {
        self.0.path.as_deref()
    }

    /// A copy without the data, e.g. to save it separately; restore it with
    /// [`Image::with_data`].
    pub fn without_data(&self) -> Image {
        self.with(|i| i.data = None)
    }

    /// A copy with the given data, which should be what
    /// [`data`](Image::data) was for this [`key`](Image::key).
    pub fn with_data(&self, data: ImageData) -> Image {
        self.with(|i| i.data = Some(data))
    }

    /// A copy without the path, e.g. to keep it private.
    pub fn without_path(&self) -> Image {
        self.with(|i| i.path = None)
    }

    /// For backends: a value kept with this image (not with copies made by
    /// methods like [`Image::without_path`]), made by `init` the first time.
    /// Typically the decoded image, ready to draw. `None` if a value of
    /// another type was stored first.
    pub fn backend_cache<T: Any + Send + Sync>(
        &self,
        init: impl FnOnce(&Image) -> T,
    ) -> Option<&T> {
        self.0
            .cache
            .get_or_init(|| Box::new(init(self)))
            .downcast_ref()
    }
}

/// Equal if made from the same data, with the same alt text, path, and
/// whether the data is there, and how it's saved.
impl PartialEq for Image {
    fn eq(&self, other: &Self) -> bool {
        let (a, b) = (&self.0, &other.0);
        Arc::ptr_eq(&self.0, &other.0)
            || (a.key == b.key
                && a.alt == b.alt
                && a.path == b.path
                && a.saving == b.saving
                && a.data.is_some() == b.data.is_some())
    }
}

impl fmt::Debug for Image {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Image")
            .field("key", &self.0.key)
            .field("alt", &self.0.alt)
            .field("data", &self.0.data)
            .field("path", &self.0.path)
            .field("saving", &self.0.saving)
            .finish()
    }
}

/// How an [`Image`] is serialized.
#[derive(Serialize, Deserialize)]
struct ImageRepr {
    key: ImageKey,
    alt: String,
    #[serde(flatten)]
    data: Option<ImageData>,
    /// Left out if the path isn't valid Unicode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    linked: bool,
}

impl Serialize for Image {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        ImageRepr {
            key: self.0.key,
            alt: self.0.alt.clone(),
            data: self.0.data.clone(),
            path: self.path().and_then(Path::to_str).map(str::to_owned),
            linked: self.is_linked(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Image {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let r = ImageRepr::deserialize(deserializer)?;
        let image = Image::new(r.key, r.alt, r.data, r.path.map(PathBuf::from));
        Ok(if r.linked { image.linked() } else { image })
    }
}

/// Identifies an [`Image`] by the data it was made from (pixels or file
/// contents), before compression: the same data gives the same key, however
/// it's compressed. A 128-bit BLAKE3 hash, serialized as hex.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ImageKey([u8; 16]);

impl ImageKey {
    fn of_rgba(width: u32, height: u32, pixels: &[u8]) -> ImageKey {
        let mut h = blake3::Hasher::new();
        h.update(b"kettle-ui rgba8\0");
        h.update(&width.to_le_bytes());
        h.update(&height.to_le_bytes());
        h.update(pixels);
        ImageKey::from_hash(h)
    }

    fn of_encoded(bytes: &[u8]) -> ImageKey {
        let mut h = blake3::Hasher::new();
        h.update(b"kettle-ui encoded\0");
        h.update(bytes);
        ImageKey::from_hash(h)
    }

    fn from_hash(h: blake3::Hasher) -> ImageKey {
        let mut key = [0; 16];
        key.copy_from_slice(&h.finalize().as_bytes()[..16]);
        ImageKey(key)
    }
}

impl fmt::Display for ImageKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

impl fmt::Debug for ImageKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ImageKey({self})")
    }
}

impl FromStr for ImageKey {
    type Err = ImageError;

    fn from_str(s: &str) -> Result<Self, ImageError> {
        let invalid = || ImageError::Invalid(format!("invalid image key {s:?}"));
        if s.len() != 32 || !s.is_ascii() {
            return Err(invalid());
        }
        let mut key = [0; 16];
        for (i, b) in key.iter_mut().enumerate() {
            *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).map_err(|_| invalid())?;
        }
        Ok(ImageKey(key))
    }
}

impl Serialize for ImageKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ImageKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// A compressed image: an image file's contents. Cheap to clone.
///
/// Images are always kept compressed. Lossy images (JPEG aside, see below)
/// are kept as they came. Lossless ones are recompressed losslessly as
/// JPEG XL, and JPEGs with JPEG XL's lossless JPEG recompression (which can
/// restore the original file exactly), whenever that's smaller.
///
/// Serialized as `format`, `width`, `height`, and `data` in base64.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageData {
    format: ImageFormat,
    width: u32,
    height: u32,
    #[serde(rename = "data", with = "base64_bytes")]
    bytes: Arc<[u8]>,
}

impl ImageData {
    /// The file format.
    pub fn format(&self) -> ImageFormat {
        self.format
    }

    /// The width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// The height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The file contents.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Decodes the image (its first frame, if animated) to 8-bit RGBA.
    pub fn decode(&self) -> Result<Rgba, ImageError> {
        codec::decode(self)
    }

    /// The original JPEG file, if the image is a JPEG, or JPEG XL made from
    /// one by lossless recompression.
    pub fn jpeg(&self) -> Result<Option<Vec<u8>>, ImageError> {
        codec::jpeg(self)
    }
}

impl fmt::Debug for ImageData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImageData")
            .field("format", &self.format)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

mod base64_bytes {
    use std::sync::Arc;

    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(bytes: &Arc<[u8]>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(bytes))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Arc<[u8]>, D::Error> {
        let s = String::deserialize(d)?;
        let bytes = STANDARD.decode(s).map_err(serde::de::Error::custom)?;
        Ok(bytes.into())
    }
}

/// The file format of [`ImageData`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ImageFormat {
    /// PNG
    Png,
    /// JPEG
    Jpeg,
    /// GIF
    Gif,
    /// WebP
    Webp,
    /// JPEG XL
    Jxl,
}

/// Decoded pixels: 8-bit RGBA (not premultiplied), rows top to bottom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rgba {
    /// The width in pixels.
    pub width: u32,
    /// The height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes.
    pub pixels: Vec<u8>,
}

/// Why an [`Image`] couldn't be made, or decoded.
#[derive(Debug)]
#[non_exhaustive]
pub enum ImageError {
    /// Reading the file failed.
    Io(io::Error),
    /// The data isn't in a supported format.
    UnknownFormat,
    /// RGBA pixels don't match the size.
    WrongSize,
    /// The image is broken.
    Invalid(String),
    /// The image isn't from a file, so it can't be read again.
    NoFile,
    /// The image's file has changed since the image was made from it.
    Changed,
}

impl fmt::Display for ImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImageError::Io(e) => write!(f, "{e}"),
            ImageError::UnknownFormat => {
                f.write_str("not an image in a supported format (PNG, JPEG, GIF, WebP or JPEG XL)")
            }
            ImageError::WrongSize => f.write_str("the pixels don't match the image size"),
            ImageError::Invalid(e) => write!(f, "invalid image: {e}"),
            ImageError::NoFile => f.write_str("the image isn't from a file"),
            ImageError::Changed => f.write_str("the image file has changed"),
        }
    }
}

impl std::error::Error for ImageError {}
