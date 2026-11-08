//! Compressing and decoding images; JPEG XL through `kettle-jxl`, other
//! formats through the `image` crate.

use std::io::Cursor;
use std::sync::Arc;

use ::image::codecs::png::{CompressionType, FilterType, PngDecoder, PngEncoder};
use ::image::metadata::Orientation;
use ::image::{
    ColorType, DynamicImage, ExtendedColorType, ImageDecoder, ImageEncoder, ImageReader,
};
use kettle_jxl::Layout;

use super::{ImageData, ImageError, ImageFormat, Rgba};

/// Compresses RGBA pixels losslessly: as JPEG XL, or PNG if that's smaller.
pub(super) fn compress_rgba(
    width: u32,
    height: u32,
    pixels: &[u8],
) -> Result<ImageData, ImageError> {
    let len = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4));
    if len != Some(pixels.len()) || width == 0 || height == 0 {
        return Err(ImageError::WrongSize);
    }
    let mut png = Vec::new();
    PngEncoder::new_with_quality(&mut png, CompressionType::Best, FilterType::Adaptive)
        .write_image(pixels, width, height, ExtendedColorType::Rgba8)
        .map_err(invalid)?;
    let layout = Layout {
        width,
        height,
        channels: 4,
        sixteen_bit: false,
    };
    let jxl = kettle_jxl::encode_lossless(pixels, layout, None);
    let png = data(ImageFormat::Png, width, height, png);
    Ok(smaller(png, jxl))
}

/// Takes an image file's contents, recompressed as JPEG XL if it's
/// lossless or a JPEG, and that's smaller.
pub(super) fn compress(bytes: Vec<u8>) -> Result<ImageData, ImageError> {
    let format = sniff(&bytes).ok_or(ImageError::UnknownFormat)?;
    let (width, height) = dimensions(format, &bytes)?;
    let jxl = match format {
        ImageFormat::Png => png_to_jxl(&bytes),
        ImageFormat::Jpeg => kettle_jxl::recompress_jpeg(&bytes).map_err(|e| e.to_string()),
        // Lossy or already JPEG XL. GIFs are kept too, in case they're
        // animated.
        ImageFormat::Gif | ImageFormat::Webp | ImageFormat::Jxl => {
            return Ok(data(format, width, height, bytes));
        }
    };
    Ok(smaller(data(format, width, height, bytes), jxl))
}

/// Takes an image file's contents as they are.
pub(super) fn take(bytes: Vec<u8>) -> Result<ImageData, ImageError> {
    let format = sniff(&bytes).ok_or(ImageError::UnknownFormat)?;
    let (width, height) = dimensions(format, &bytes)?;
    Ok(data(format, width, height, bytes))
}

fn data(format: ImageFormat, width: u32, height: u32, bytes: Vec<u8>) -> ImageData {
    ImageData {
        format,
        width,
        height,
        bytes: Arc::from(bytes),
    }
}

/// `original`, or `jxl` (of the same image) if it's smaller.
fn smaller<E: ToString>(original: ImageData, jxl: Result<Vec<u8>, E>) -> ImageData {
    match jxl {
        Ok(jxl) if jxl.len() < original.bytes.len() => {
            data(ImageFormat::Jxl, original.width, original.height, jxl)
        }
        Ok(_) => original,
        Err(e) => {
            log::debug!(
                "kept a {:?} image, recompressing as JPEG XL failed: {}",
                original.format,
                e.to_string()
            );
            original
        }
    }
}

/// Recompresses a (non-animated) PNG losslessly, keeping its bit depth and
/// color profile.
fn png_to_jxl(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut decoder = PngDecoder::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
    if decoder.is_apng().map_err(|e| e.to_string())? {
        return Err("animated PNGs are kept".into());
    }
    let icc = decoder.icc_profile().map_err(|e| e.to_string())?;
    let (width, height) = decoder.dimensions();
    let (channels, sixteen_bit) = match decoder.color_type() {
        ColorType::L8 => (1, false),
        ColorType::La8 => (2, false),
        ColorType::Rgb8 => (3, false),
        ColorType::Rgba8 => (4, false),
        ColorType::L16 => (1, true),
        ColorType::La16 => (2, true),
        ColorType::Rgb16 => (3, true),
        ColorType::Rgba16 => (4, true),
        other => return Err(format!("unsupported color type {other:?}")),
    };
    // 16-bit samples come in native byte order, as libjxl takes them.
    let mut pixels = vec![0; decoder.total_bytes() as usize];
    decoder.read_image(&mut pixels).map_err(|e| e.to_string())?;
    let layout = Layout {
        width,
        height,
        channels,
        sixteen_bit,
    };
    kettle_jxl::encode_lossless(&pixels, layout, icc.as_deref()).map_err(|e| e.to_string())
}

/// The format, from the first bytes.
fn sniff(bytes: &[u8]) -> Option<ImageFormat> {
    const JXL_CONTAINER: &[u8] = b"\0\0\0\x0cJXL \r\n\x87\n";
    Some(if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        ImageFormat::Png
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        ImageFormat::Jpeg
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        ImageFormat::Gif
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        ImageFormat::Webp
    } else if bytes.starts_with(b"\xff\x0a") || bytes.starts_with(JXL_CONTAINER) {
        ImageFormat::Jxl
    } else {
        return None;
    })
}

fn image_crate_format(format: ImageFormat) -> ::image::ImageFormat {
    match format {
        ImageFormat::Png => ::image::ImageFormat::Png,
        ImageFormat::Jpeg => ::image::ImageFormat::Jpeg,
        ImageFormat::Gif => ::image::ImageFormat::Gif,
        ImageFormat::Webp => ::image::ImageFormat::WebP,
        ImageFormat::Jxl => unreachable!("JPEG XL is decoded by libjxl"),
    }
}

/// The size of the image as shown, after orientation (from Exif).
fn dimensions(format: ImageFormat, bytes: &[u8]) -> Result<(u32, u32), ImageError> {
    if format == ImageFormat::Jxl {
        return kettle_jxl::dimensions(bytes).map_err(invalid);
    }
    let mut decoder = ImageReader::with_format(Cursor::new(bytes), image_crate_format(format))
        .into_decoder()
        .map_err(invalid)?;
    let (width, height) = decoder.dimensions();
    let orientation = decoder.orientation().map_err(invalid)?;
    Ok(if rotates(orientation) {
        (height, width)
    } else {
        (width, height)
    })
}

/// Whether an orientation swaps width and height.
fn rotates(orientation: Orientation) -> bool {
    matches!(
        orientation,
        Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270FlipH
    )
}

pub(super) fn decode(data: &ImageData) -> Result<Rgba, ImageError> {
    if data.format == ImageFormat::Jxl {
        let rgba = kettle_jxl::decode_rgba(&data.bytes).map_err(invalid)?;
        return Ok(Rgba {
            width: rgba.width,
            height: rgba.height,
            pixels: rgba.pixels,
        });
    }
    let reader = ImageReader::with_format(
        Cursor::new(&data.bytes[..]),
        image_crate_format(data.format),
    );
    let mut decoder = reader.into_decoder().map_err(invalid)?;
    let orientation = decoder.orientation().map_err(invalid)?;
    let mut image = DynamicImage::from_decoder(decoder).map_err(invalid)?;
    image.apply_orientation(orientation);
    let image = image.into_rgba8();
    Ok(Rgba {
        width: image.width(),
        height: image.height(),
        pixels: image.into_raw(),
    })
}

pub(super) fn jpeg(data: &ImageData) -> Result<Option<Vec<u8>>, ImageError> {
    match data.format {
        ImageFormat::Jpeg => Ok(Some(data.bytes.to_vec())),
        ImageFormat::Jxl => kettle_jxl::reconstruct_jpeg(&data.bytes).map_err(invalid),
        _ => Ok(None),
    }
}

fn invalid(e: impl ToString) -> ImageError {
    ImageError::Invalid(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Image;

    /// A smooth gradient, which compresses well losslessly.
    pub(crate) fn gradient(width: u32, height: u32) -> Vec<u8> {
        let mut pixels = Vec::new();
        for y in 0..height {
            for x in 0..width {
                pixels.extend([(x * 4) as u8, (y * 4) as u8, 128, 255]);
            }
        }
        pixels
    }

    fn png(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
        let mut png = Vec::new();
        PngEncoder::new_with_quality(&mut png, CompressionType::Fast, FilterType::NoFilter)
            .write_image(pixels, width, height, ExtendedColorType::Rgba8)
            .unwrap();
        png
    }

    fn jpeg(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
        let rgb = DynamicImage::ImageRgba8(
            ::image::RgbaImage::from_raw(width, height, pixels.to_vec()).unwrap(),
        )
        .into_rgb8();
        let mut jpeg = Vec::new();
        rgb.write_to(&mut Cursor::new(&mut jpeg), ::image::ImageFormat::Jpeg)
            .unwrap();
        jpeg
    }

    #[test]
    fn rgba_is_compressed_losslessly() {
        let pixels = gradient(50, 40);
        let image = Image::from_rgba(50, 40, &pixels, "gradient").unwrap();
        let data = image.data().unwrap();
        assert_eq!(data.format(), ImageFormat::Jxl);
        assert_eq!((data.width(), data.height()), (50, 40));
        assert!(data.bytes().len() < pixels.len());
        assert_eq!(data.decode().unwrap().pixels, pixels);
        assert!(matches!(
            Image::from_rgba(50, 41, &pixels, ""),
            Err(ImageError::WrongSize)
        ));
    }

    #[test]
    fn png_is_recompressed_losslessly_if_smaller() {
        let pixels = gradient(64, 64);
        let png = png(64, 64, &pixels);
        let image = Image::from_bytes(png.clone(), "").unwrap();
        let data = image.data().unwrap();
        assert_eq!(data.format(), ImageFormat::Jxl);
        assert!(data.bytes().len() < png.len());
        assert_eq!(data.decode().unwrap().pixels, pixels);
        // The key is from the PNG, whatever it's compressed to.
        assert_eq!(image.key(), Image::from_bytes(png, "x").unwrap().key());
        assert_ne!(
            image.key(),
            Image::from_rgba(64, 64, &pixels, "").unwrap().key()
        );
    }

    #[test]
    fn jpeg_is_recompressed_exactly() {
        let jpeg = jpeg(64, 48, &gradient(64, 48));
        let image = Image::from_bytes(jpeg.clone(), "").unwrap();
        let data = image.data().unwrap();
        assert_eq!(data.format(), ImageFormat::Jxl);
        assert!(data.bytes().len() < jpeg.len());
        assert_eq!((data.width(), data.height()), (64, 48));
        // Decodes to nearly the JPEG's pixels (decoders differ slightly in
        // rounding and upsampling).
        let original = decode(&super::data(ImageFormat::Jpeg, 64, 48, jpeg.clone())).unwrap();
        let decoded = data.decode().unwrap();
        assert_eq!(original.pixels.len(), decoded.pixels.len());
        let total: u32 = (original.pixels.iter().zip(&decoded.pixels))
            .map(|(a, b)| u32::from(a.abs_diff(*b)))
            .sum();
        let mean = total as f64 / original.pixels.len() as f64;
        assert!(mean < 1.0, "{mean}");
        assert_eq!(data.jpeg().unwrap(), Some(jpeg));
        let png = Image::from_rgba(2, 2, &gradient(2, 2), "").unwrap();
        assert_eq!(png.data().unwrap().jpeg().unwrap(), None);
    }

    #[test]
    fn other_formats_are_kept() {
        let rgba = ::image::RgbaImage::from_raw(8, 8, gradient(8, 8)).unwrap();
        let mut gif = Vec::new();
        rgba.write_to(&mut Cursor::new(&mut gif), ::image::ImageFormat::Gif)
            .unwrap();
        let image = Image::from_bytes(gif.clone(), "").unwrap();
        assert_eq!(image.data().unwrap().format(), ImageFormat::Gif);
        assert_eq!(image.data().unwrap().bytes(), gif);
        assert_eq!(image.data().unwrap().decode().unwrap().width, 8);
    }

    #[test]
    fn unknown_and_broken_data_are_errors() {
        assert!(matches!(
            Image::from_bytes(b"hello".to_vec(), ""),
            Err(ImageError::UnknownFormat)
        ));
        assert!(matches!(
            Image::from_bytes(b"\x89PNG\r\n\x1a\nbroken".to_vec(), ""),
            Err(ImageError::Invalid(_))
        ));
        assert!(matches!(
            Image::from_file("/nonexistent/image.png", ""),
            Err(ImageError::Io(_))
        ));
    }

    #[test]
    fn files_keep_their_path() {
        let dir = std::env::temp_dir().join(format!("kettle-ui-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.png");
        let png = png(4, 4, &gradient(4, 4));
        std::fs::write(&path, &png).unwrap();
        let image = Image::from_file(&path, "a").unwrap();
        assert_eq!(image.path(), Some(path.as_path()));
        assert_eq!(image.key(), Image::from_bytes(png, "").unwrap().key());
        assert_eq!(image.without_path().path(), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn linked_images_are_reread_if_unchanged() {
        let dir = std::env::temp_dir().join(format!("kettle-ui-reread-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.png");
        let original = png(4, 4, &gradient(4, 4));
        std::fs::write(&path, &original).unwrap();
        let image = Image::from_file(&path, "a").unwrap().linked();
        let reread = image.without_data().reread().unwrap();
        assert!(reread.is_linked());
        assert_eq!(reread.data().unwrap().bytes(), original);

        std::fs::write(&path, png(4, 4, &[0; 64])).unwrap();
        assert!(matches!(image.reread(), Err(ImageError::Changed)));
        assert!(matches!(
            Image::from_bytes(original, "").unwrap().reread(),
            Err(ImageError::NoFile)
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
