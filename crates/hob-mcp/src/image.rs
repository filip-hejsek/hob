//! Images for the agent: in a format MCP clients take (PNG, JPEG, GIF or
//! WebP), and not too big.

use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::imageops::FilterType;
use image::{ExtendedColorType, ImageEncoder, RgbImage, RgbaImage};
use kettle_ui::widget::{Kind, Widget, is_shown};
use kettle_ui::{FrozenIsland, Image, ImageData, ImageFormat, Value};

/// The longest side an image is sent with; larger ones are scaled down.
/// Models scale them down to about this anyway.
const MAX_SIDE: u32 = 1568;

/// Images larger than this are converted (and then sent as JPEG, if PNG is
/// larger too).
const MAX_BYTES: usize = 1 << 20;

/// The quality of JPEGs made for the agent.
const JPEG_QUALITY: u8 = 85;

/// An image file for the agent.
#[derive(Debug)]
pub struct AgentImage {
    /// Its media type, e.g. `image/png`.
    pub mime: &'static str,
    /// The file contents.
    pub bytes: Vec<u8>,
}

/// The images an island shows, in the order of its outline; each once.
pub fn shown_images(island: &FrozenIsland) -> Vec<Image> {
    let mut images: Vec<Image> = Vec::new();
    collect(island, island.root(), &mut images);
    images
}

fn collect(island: &FrozenIsland, widget: &Widget, images: &mut Vec<Image>) {
    match widget.kind() {
        Kind::Column(children) | Kind::Row(children) => {
            for child in children {
                collect(island, child, images);
            }
        }
        Kind::Table { rows, .. } => {
            for cell in rows.iter().flatten() {
                collect(island, cell, images);
            }
        }
        // As in the outline, hidden children aren't shown.
        Kind::When { state, child } => {
            if is_shown(island, state) {
                collect(island, child, images);
            }
        }
        Kind::Image { state } => {
            if let Some(Value::Image(image)) = island.state(state)
                && !images.iter().any(|i| i.key() == image.key())
            {
                images.push(image.clone());
            }
        }
        _ => {}
    }
}

/// The image as the agent gets it: as it is if MCP clients take its format
/// and it's small enough (a JPEG recompressed as JPEG XL as the original
/// JPEG), otherwise decoded, scaled down to at most 1568 px, and sent as
/// PNG (or JPEG, if the PNG is over 1 MiB).
pub fn for_agent(data: &ImageData) -> Result<AgentImage, String> {
    let mime = match data.format() {
        ImageFormat::Png => Some("image/png"),
        ImageFormat::Jpeg => Some("image/jpeg"),
        ImageFormat::Gif => Some("image/gif"),
        ImageFormat::Webp => Some("image/webp"),
        _ => None,
    };
    if data.width().max(data.height()) <= MAX_SIDE {
        if let Some(mime) = mime
            && data.bytes().len() <= MAX_BYTES
        {
            let bytes = data.bytes().to_vec();
            return Ok(AgentImage { mime, bytes });
        }
        // If it fails, decoding below reports what's wrong.
        if data.format() == ImageFormat::Jxl
            && let Ok(Some(jpeg)) = data.jpeg()
            && jpeg.len() <= MAX_BYTES
        {
            let mime = "image/jpeg";
            return Ok(AgentImage { mime, bytes: jpeg });
        }
    }
    let rgba = data.decode().map_err(|e| e.to_string())?;
    let mut image = RgbaImage::from_raw(rgba.width, rgba.height, rgba.pixels)
        .ok_or("decoded pixels don't match the size")?;
    if image.width().max(image.height()) > MAX_SIDE {
        image = scale_down(&image);
    }
    let (width, height) = image.dimensions();
    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(&image, width, height, ExtendedColorType::Rgba8)
        .map_err(|e| e.to_string())?;
    if png.len() <= MAX_BYTES {
        return Ok(AgentImage {
            mime: "image/png",
            bytes: png,
        });
    }
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(Cursor::new(&mut jpeg), JPEG_QUALITY)
        .write_image(&on_white(&image), width, height, ExtendedColorType::Rgb8)
        .map_err(|e| e.to_string())?;
    Ok(AgentImage {
        mime: "image/jpeg",
        bytes: jpeg,
    })
}

/// Scales an image down so that its longer side is [`MAX_SIDE`].
fn scale_down(image: &RgbaImage) -> RgbaImage {
    let (width, height) = image.dimensions();
    let scale = |side: u32| {
        let scaled = u64::from(side) * u64::from(MAX_SIDE) / u64::from(width.max(height));
        (scaled as u32).max(1)
    };
    image::imageops::resize(image, scale(width), scale(height), FilterType::Triangle)
}

/// The image over a white background, without alpha, for JPEG.
fn on_white(image: &RgbaImage) -> RgbImage {
    RgbImage::from_fn(image.width(), image.height(), |x, y| {
        let [r, g, b, a] = image.get_pixel(x, y).0;
        let blend =
            |c: u8| ((u32::from(c) * u32::from(a) + 255 * (255 - u32::from(a))) / 255) as u8;
        image::Rgb([blend(r), blend(g), blend(b)])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoded(image: &AgentImage) -> RgbaImage {
        image::load_from_memory(&image.bytes).unwrap().into_rgba8()
    }

    /// Pixels that don't compress well: noise.
    fn noise(width: u32, height: u32) -> Vec<u8> {
        let mut state = 1u32;
        let mut pixels = Vec::new();
        for _ in 0..width * height {
            for _ in 0..3 {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                pixels.push((state >> 24) as u8);
            }
            pixels.push(255);
        }
        pixels
    }

    #[test]
    fn jpeg_xl_is_sent_as_png() {
        let pixels: Vec<u8> = (0..40 * 30).flat_map(|i| [i as u8, 0, 128, 255]).collect();
        let image = Image::from_rgba(40, 30, &pixels, "").unwrap();
        let data = image.data().unwrap();
        assert_eq!(data.format(), ImageFormat::Jxl);
        let sent = for_agent(data).unwrap();
        assert_eq!(sent.mime, "image/png");
        assert_eq!(decoded(&sent).into_raw(), pixels);
    }

    #[test]
    fn recompressed_jpegs_are_sent_as_the_original() {
        let rgb = RgbImage::from_fn(64, 48, |x, y| image::Rgb([x as u8 * 4, y as u8 * 5, 128]));
        let mut jpeg = Vec::new();
        JpegEncoder::new_with_quality(Cursor::new(&mut jpeg), 90)
            .write_image(&rgb, 64, 48, ExtendedColorType::Rgb8)
            .unwrap();
        let image = Image::from_bytes(jpeg.clone(), "").unwrap();
        let data = image.data().unwrap();
        assert_eq!(data.format(), ImageFormat::Jxl);
        let sent = for_agent(data).unwrap();
        assert_eq!((sent.mime, sent.bytes), ("image/jpeg", jpeg));
    }

    #[test]
    fn small_images_are_sent_as_they_are() {
        // Made directly, since a PNG this small would be recompressed.
        let mut png = Vec::new();
        PngEncoder::new(&mut png)
            .write_image(&noise(4, 3), 4, 3, ExtendedColorType::Rgba8)
            .unwrap();
        let data: ImageData = serde_json::from_value(serde_json::json!({
            "format": "png",
            "width": 4,
            "height": 3,
            "data": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &png),
        }))
        .unwrap();
        let sent = for_agent(&data).unwrap();
        assert_eq!((sent.mime, &sent.bytes[..]), ("image/png", &png[..]));
    }

    #[test]
    fn big_images_are_scaled_down() {
        let image = Image::from_rgba(2000, 10, &[0, 0, 255, 255].repeat(20_000), "").unwrap();
        let sent = for_agent(image.data().unwrap()).unwrap();
        assert_eq!(decoded(&sent).dimensions(), (MAX_SIDE, 7));
    }

    #[test]
    fn images_too_big_for_png_are_sent_as_jpeg() {
        let image = Image::from_rgba(1000, 1000, &noise(1000, 1000), "").unwrap();
        let sent = for_agent(image.data().unwrap()).unwrap();
        assert_eq!(sent.mime, "image/jpeg");
        assert_eq!(decoded(&sent).dimensions(), (1000, 1000));
    }
}
