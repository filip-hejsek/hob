//! Minimal bindings to the system libjxl (the JPEG XL reference library),
//! for kettle-ui images: lossless encoding, lossless JPEG recompression
//! (and reconstructing the JPEG), and decoding to 8-bit RGBA.
//!
//! The bindings are written by hand, for just the functions used here
//! (the only maintained binding crate is GPL). The structs match libjxl's
//! headers, which keep them ABI-compatible across versions.

#![allow(unsafe_code)]

use std::ffi::c_void;
use std::fmt;
use std::ptr;

/// Why encoding or decoding failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

fn error(what: &str) -> Error {
    Error(format!("libjxl: {what} failed"))
}

/// The layout of pixels handed to [`encode_lossless`]: rows top to bottom,
/// no padding, channels interleaved.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// 1 (gray), 2 (gray and alpha), 3 (RGB) or 4 (RGBA).
    pub channels: u32,
    /// Whether samples are 16-bit (in native byte order) rather than 8-bit.
    pub sixteen_bit: bool,
}

impl Layout {
    fn byte_len(&self) -> Option<usize> {
        let sample = if self.sixteen_bit { 2 } else { 1 };
        (self.width as usize)
            .checked_mul(self.height as usize)?
            .checked_mul(self.channels as usize)?
            .checked_mul(sample)
    }
}

/// Encodes pixels losslessly. `icc` is the color profile; without one, the
/// pixels are taken to be sRGB.
pub fn encode_lossless(
    pixels: &[u8],
    layout: Layout,
    icc: Option<&[u8]>,
) -> Result<Vec<u8>, Error> {
    if !(1..=4).contains(&layout.channels) || layout.width == 0 || layout.height == 0 {
        return Err(Error("libjxl: unsupported pixel layout".into()));
    }
    if layout.byte_len() != Some(pixels.len()) {
        return Err(Error("libjxl: pixel data doesn't match the size".into()));
    }
    let bits = if layout.sixteen_bit { 16 } else { 8 };
    let has_alpha = layout.channels.is_multiple_of(2);
    let enc = Encoder::new()?;
    // SAFETY: `enc` is a live encoder; all pointers passed are valid for the
    // duration of each call, and libjxl copies what it keeps.
    unsafe {
        let mut info: ffi::JxlBasicInfo = std::mem::zeroed();
        ffi::JxlEncoderInitBasicInfo(&mut info);
        info.xsize = layout.width;
        info.ysize = layout.height;
        info.bits_per_sample = bits;
        info.uses_original_profile = ffi::TRUE;
        info.num_color_channels = if layout.channels >= 3 { 3 } else { 1 };
        if has_alpha {
            info.num_extra_channels = 1;
            info.alpha_bits = bits;
        }
        check(
            ffi::JxlEncoderSetBasicInfo(enc.0, &info),
            "setting the basic info",
        )?;
        match icc {
            Some(icc) => check(
                ffi::JxlEncoderSetICCProfile(enc.0, icc.as_ptr(), icc.len()),
                "setting the ICC profile",
            )?,
            None => {
                let mut color: ffi::JxlColorEncoding = std::mem::zeroed();
                let gray = if layout.channels < 3 {
                    ffi::TRUE
                } else {
                    ffi::FALSE
                };
                ffi::JxlColorEncodingSetToSRGB(&mut color, gray);
                check(
                    ffi::JxlEncoderSetColorEncoding(enc.0, &color),
                    "setting the color encoding",
                )?;
            }
        }
        let settings = enc.frame_settings()?;
        check(
            ffi::JxlEncoderSetFrameLossless(settings, ffi::TRUE),
            "making the frame lossless",
        )?;
        let format = ffi::JxlPixelFormat {
            num_channels: layout.channels,
            data_type: if layout.sixteen_bit {
                ffi::TYPE_UINT16
            } else {
                ffi::TYPE_UINT8
            },
            endianness: ffi::NATIVE_ENDIAN,
            align: 0,
        };
        check(
            ffi::JxlEncoderAddImageFrame(settings, &format, pixels.as_ptr().cast(), pixels.len()),
            "adding the image",
        )?;
    }
    enc.finish()
}

/// Recompresses a JPEG file losslessly: the result decodes to the same
/// pixels, and the original file can be reconstructed exactly from it.
pub fn recompress_jpeg(jpeg: &[u8]) -> Result<Vec<u8>, Error> {
    let enc = Encoder::new()?;
    // SAFETY: as in `encode_lossless`.
    unsafe {
        check(
            ffi::JxlEncoderStoreJPEGMetadata(enc.0, ffi::TRUE),
            "storing JPEG metadata",
        )?;
        let settings = enc.frame_settings()?;
        check(
            ffi::JxlEncoderAddJPEGFrame(settings, jpeg.as_ptr(), jpeg.len()),
            "adding the JPEG",
        )?;
    }
    enc.finish()
}

/// Decoded pixels: 8-bit RGBA, rows top to bottom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rgba {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes.
    pub pixels: Vec<u8>,
}

/// The size of an image, after orientation, from its header.
pub fn dimensions(jxl: &[u8]) -> Result<(u32, u32), Error> {
    let dec = Decoder::new(jxl, ffi::DEC_BASIC_INFO)?;
    // SAFETY: `dec` is a live decoder whose input outlives it.
    match unsafe { ffi::JxlDecoderProcessInput(dec.dec) } {
        ffi::DEC_BASIC_INFO => {
            let info = dec.basic_info()?;
            Ok((info.xsize, info.ysize))
        }
        _ => Err(error("reading the header")),
    }
}

/// Decodes an image (its first frame, if animated) to 8-bit RGBA.
pub fn decode_rgba(jxl: &[u8]) -> Result<Rgba, Error> {
    let dec = Decoder::new(jxl, ffi::DEC_BASIC_INFO | ffi::DEC_FULL_IMAGE)?;
    let format = ffi::JxlPixelFormat {
        num_channels: 4,
        data_type: ffi::TYPE_UINT8,
        endianness: ffi::NATIVE_ENDIAN,
        align: 0,
    };
    let mut size = None;
    let mut pixels = Vec::new();
    loop {
        // SAFETY: `dec` is a live decoder whose input outlives it; `pixels`
        // is handed to it with its exact length, and isn't touched until
        // decoding ends.
        match unsafe { ffi::JxlDecoderProcessInput(dec.dec) } {
            ffi::DEC_BASIC_INFO => {
                let info = dec.basic_info()?;
                size = Some((info.xsize, info.ysize));
            }
            ffi::DEC_NEED_IMAGE_OUT_BUFFER => {
                let (width, height) = size.ok_or_else(|| error("reading the header"))?;
                let mut len = 0;
                unsafe {
                    check_dec(
                        ffi::JxlDecoderImageOutBufferSize(dec.dec, &format, &mut len),
                        "sizing the output",
                    )?;
                }
                if Some(len) != (width as usize * height as usize).checked_mul(4) {
                    return Err(error("sizing the output"));
                }
                pixels = vec![0; len];
                unsafe {
                    check_dec(
                        ffi::JxlDecoderSetImageOutBuffer(
                            dec.dec,
                            &format,
                            pixels.as_mut_ptr().cast(),
                            len,
                        ),
                        "setting the output",
                    )?;
                }
            }
            // The first frame is all we need.
            ffi::DEC_FULL_IMAGE | ffi::DEC_SUCCESS => {
                let (width, height) = size.ok_or_else(|| error("decoding"))?;
                if pixels.is_empty() {
                    return Err(error("decoding"));
                }
                return Ok(Rgba {
                    width,
                    height,
                    pixels,
                });
            }
            _ => return Err(error("decoding")),
        }
    }
}

/// The JPEG file an image was made from by [`recompress_jpeg`], exactly;
/// `None` if it wasn't made from a JPEG.
pub fn reconstruct_jpeg(jxl: &[u8]) -> Result<Option<Vec<u8>>, Error> {
    // Recompression saves about a fifth, so this usually fits.
    reconstruct_jpeg_into((64 * 1024).max(jxl.len() * 2), jxl)
}

/// [`reconstruct_jpeg`], starting with a buffer of `len` bytes.
fn reconstruct_jpeg_into(len: usize, jxl: &[u8]) -> Result<Option<Vec<u8>>, Error> {
    let dec = Decoder::new(jxl, ffi::DEC_JPEG_RECONSTRUCTION | ffi::DEC_FULL_IMAGE)?;
    let mut jpeg: Vec<u8> = Vec::new();
    let mut started = false;
    loop {
        // SAFETY: `dec` is a live decoder whose input outlives it. The
        // unwritten part of `jpeg` is handed to it with its exact length,
        // and released before `jpeg` is resized or read.
        match unsafe { ffi::JxlDecoderProcessInput(dec.dec) } {
            ffi::DEC_JPEG_RECONSTRUCTION => {
                jpeg = vec![0; len.max(1)];
                started = true;
                unsafe {
                    check_dec(
                        ffi::JxlDecoderSetJPEGBuffer(dec.dec, jpeg.as_mut_ptr(), jpeg.len()),
                        "setting the JPEG output",
                    )?;
                }
            }
            ffi::DEC_JPEG_NEED_MORE_OUTPUT => {
                let unused = unsafe { ffi::JxlDecoderReleaseJPEGBuffer(dec.dec) };
                let done = jpeg.len() - unused;
                jpeg.resize(jpeg.len() * 2, 0);
                unsafe {
                    check_dec(
                        ffi::JxlDecoderSetJPEGBuffer(
                            dec.dec,
                            jpeg.as_mut_ptr().add(done),
                            jpeg.len() - done,
                        ),
                        "setting the JPEG output",
                    )?;
                }
            }
            // Without reconstruction data, the decoder wants pixels instead.
            ffi::DEC_NEED_IMAGE_OUT_BUFFER if !started => return Ok(None),
            ffi::DEC_FULL_IMAGE | ffi::DEC_SUCCESS => {
                if !started {
                    return Ok(None);
                }
                let unused = unsafe { ffi::JxlDecoderReleaseJPEGBuffer(dec.dec) };
                jpeg.truncate(jpeg.len() - unused);
                return Ok(Some(jpeg));
            }
            _ => return Err(error("reconstructing the JPEG")),
        }
    }
}

fn check(status: ffi::JxlEncoderStatus, what: &str) -> Result<(), Error> {
    match status {
        ffi::ENC_SUCCESS => Ok(()),
        _ => Err(error(what)),
    }
}

fn check_dec(status: ffi::JxlDecoderStatus, what: &str) -> Result<(), Error> {
    match status {
        ffi::DEC_SUCCESS => Ok(()),
        _ => Err(error(what)),
    }
}

/// libjxl's thread pool, with its default number of threads.
struct Runner(*mut c_void);

impl Runner {
    fn new() -> Result<Self, Error> {
        // SAFETY: plain calls; a null memory manager means malloc.
        let runner = unsafe {
            let threads = ffi::JxlThreadParallelRunnerDefaultNumWorkerThreads();
            ffi::JxlThreadParallelRunnerCreate(ptr::null(), threads)
        };
        if runner.is_null() {
            return Err(error("creating the thread pool"));
        }
        Ok(Runner(runner))
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        // SAFETY: created by `JxlThreadParallelRunnerCreate`, destroyed once,
        // after the encoder or decoder using it (field order).
        unsafe { ffi::JxlThreadParallelRunnerDestroy(self.0) }
    }
}

/// An encoder, with its thread pool. Fields drop in order: the encoder
/// before the pool.
struct Encoder(*mut ffi::JxlEncoder, Runner);

impl Encoder {
    fn new() -> Result<Self, Error> {
        let runner = Runner::new()?;
        // SAFETY: plain call; null means malloc.
        let enc = unsafe { ffi::JxlEncoderCreate(ptr::null()) };
        if enc.is_null() {
            return Err(error("creating the encoder"));
        }
        let enc = Encoder(enc, runner);
        // SAFETY: both are live, and the runner outlives the encoder.
        unsafe {
            check(
                ffi::JxlEncoderSetParallelRunner(enc.0, ffi::JxlThreadParallelRunner, enc.1.0),
                "setting the thread pool",
            )?;
        }
        Ok(enc)
    }

    /// Settings for the one frame; owned by the encoder.
    fn frame_settings(&self) -> Result<*mut ffi::JxlEncoderFrameSettings, Error> {
        // SAFETY: the encoder is live.
        let settings = unsafe { ffi::JxlEncoderFrameSettingsCreate(self.0, ptr::null()) };
        if settings.is_null() {
            return Err(error("creating frame settings"));
        }
        Ok(settings)
    }

    /// Closes the input and collects the output.
    fn finish(self) -> Result<Vec<u8>, Error> {
        let mut out = vec![0u8; 64 * 1024];
        let mut used = 0;
        // SAFETY: `next` and `avail` describe the unused part of `out`.
        unsafe {
            ffi::JxlEncoderCloseInput(self.0);
            loop {
                let mut next = out.as_mut_ptr().add(used);
                let mut avail = out.len() - used;
                let status = ffi::JxlEncoderProcessOutput(self.0, &mut next, &mut avail);
                used = out.len() - avail;
                match status {
                    ffi::ENC_SUCCESS => break,
                    ffi::ENC_NEED_MORE_OUTPUT => out.resize(out.len() * 2, 0),
                    _ => return Err(error("encoding")),
                }
            }
        }
        out.truncate(used);
        Ok(out)
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: created by `JxlEncoderCreate`, destroyed once.
        unsafe { ffi::JxlEncoderDestroy(self.0) }
    }
}

/// A decoder reading `input`, with its thread pool.
struct Decoder<'a> {
    dec: *mut ffi::JxlDecoder,
    _runner: Runner,
    _input: &'a [u8],
}

impl<'a> Decoder<'a> {
    fn new(input: &'a [u8], events: i32) -> Result<Self, Error> {
        let runner = Runner::new()?;
        // SAFETY: plain call; null means malloc.
        let dec = unsafe { ffi::JxlDecoderCreate(ptr::null()) };
        if dec.is_null() {
            return Err(error("creating the decoder"));
        }
        let dec = Decoder {
            dec,
            _runner: runner,
            _input: input,
        };
        // SAFETY: the decoder is live; the runner and input outlive it.
        unsafe {
            check_dec(
                ffi::JxlDecoderSubscribeEvents(dec.dec, events),
                "subscribing to events",
            )?;
            check_dec(
                ffi::JxlDecoderSetParallelRunner(
                    dec.dec,
                    ffi::JxlThreadParallelRunner,
                    dec._runner.0,
                ),
                "setting the thread pool",
            )?;
            check_dec(
                ffi::JxlDecoderSetInput(dec.dec, input.as_ptr(), input.len()),
                "setting the input",
            )?;
            ffi::JxlDecoderCloseInput(dec.dec);
        }
        Ok(dec)
    }

    fn basic_info(&self) -> Result<ffi::JxlBasicInfo, Error> {
        // SAFETY: the decoder is live; `info` is plain data.
        unsafe {
            let mut info: ffi::JxlBasicInfo = std::mem::zeroed();
            check_dec(
                ffi::JxlDecoderGetBasicInfo(self.dec, &mut info),
                "reading the header",
            )?;
            Ok(info)
        }
    }
}

impl Drop for Decoder<'_> {
    fn drop(&mut self) {
        // SAFETY: created by `JxlDecoderCreate`, destroyed once, before the
        // runner (field order).
        unsafe { ffi::JxlDecoderDestroy(self.dec) }
    }
}

/// Declarations from libjxl's headers (`jxl/encode.h`, `jxl/decode.h`,
/// `jxl/thread_parallel_runner.h`), for just what's used.
#[allow(non_snake_case, non_camel_case_types, dead_code)]
mod ffi {
    use std::ffi::c_void;

    pub(crate) type JXL_BOOL = i32;
    pub(crate) const TRUE: JXL_BOOL = 1;
    pub(crate) const FALSE: JXL_BOOL = 0;

    pub(crate) type JxlEncoderStatus = i32;
    pub(crate) const ENC_SUCCESS: JxlEncoderStatus = 0;
    pub(crate) const ENC_NEED_MORE_OUTPUT: JxlEncoderStatus = 2;

    pub(crate) type JxlDecoderStatus = i32;
    pub(crate) const DEC_SUCCESS: JxlDecoderStatus = 0;
    pub(crate) const DEC_NEED_IMAGE_OUT_BUFFER: JxlDecoderStatus = 5;
    pub(crate) const DEC_JPEG_NEED_MORE_OUTPUT: JxlDecoderStatus = 6;
    pub(crate) const DEC_BASIC_INFO: JxlDecoderStatus = 0x40;
    pub(crate) const DEC_FULL_IMAGE: JxlDecoderStatus = 0x1000;
    pub(crate) const DEC_JPEG_RECONSTRUCTION: JxlDecoderStatus = 0x2000;

    pub(crate) type JxlDataType = i32;
    pub(crate) const TYPE_UINT8: JxlDataType = 2;
    pub(crate) const TYPE_UINT16: JxlDataType = 3;

    pub(crate) type JxlEndianness = i32;
    pub(crate) const NATIVE_ENDIAN: JxlEndianness = 0;

    #[repr(C)]
    pub(crate) struct JxlEncoder {
        _private: [u8; 0],
    }

    #[repr(C)]
    pub(crate) struct JxlEncoderFrameSettings {
        _private: [u8; 0],
    }

    #[repr(C)]
    pub(crate) struct JxlDecoder {
        _private: [u8; 0],
    }

    /// Not used: always null (malloc).
    #[repr(C)]
    pub(crate) struct JxlMemoryManager {
        _private: [u8; 0],
    }

    #[repr(C)]
    pub(crate) struct JxlPixelFormat {
        pub(crate) num_channels: u32,
        pub(crate) data_type: JxlDataType,
        pub(crate) endianness: JxlEndianness,
        pub(crate) align: usize,
    }

    #[repr(C)]
    pub(crate) struct JxlPreviewHeader {
        pub(crate) xsize: u32,
        pub(crate) ysize: u32,
    }

    #[repr(C)]
    pub(crate) struct JxlAnimationHeader {
        pub(crate) tps_numerator: u32,
        pub(crate) tps_denominator: u32,
        pub(crate) num_loops: u32,
        pub(crate) have_timecodes: JXL_BOOL,
    }

    #[repr(C)]
    pub(crate) struct JxlBasicInfo {
        pub(crate) have_container: JXL_BOOL,
        pub(crate) xsize: u32,
        pub(crate) ysize: u32,
        pub(crate) bits_per_sample: u32,
        pub(crate) exponent_bits_per_sample: u32,
        pub(crate) intensity_target: f32,
        pub(crate) min_nits: f32,
        pub(crate) relative_to_max_display: JXL_BOOL,
        pub(crate) linear_below: f32,
        pub(crate) uses_original_profile: JXL_BOOL,
        pub(crate) have_preview: JXL_BOOL,
        pub(crate) have_animation: JXL_BOOL,
        pub(crate) orientation: i32,
        pub(crate) num_color_channels: u32,
        pub(crate) num_extra_channels: u32,
        pub(crate) alpha_bits: u32,
        pub(crate) alpha_exponent_bits: u32,
        pub(crate) alpha_premultiplied: JXL_BOOL,
        pub(crate) preview: JxlPreviewHeader,
        pub(crate) animation: JxlAnimationHeader,
        pub(crate) intrinsic_xsize: u32,
        pub(crate) intrinsic_ysize: u32,
        pub(crate) padding: [u8; 100],
    }

    #[repr(C)]
    pub(crate) struct JxlColorEncoding {
        pub(crate) color_space: i32,
        pub(crate) white_point: i32,
        pub(crate) white_point_xy: [f64; 2],
        pub(crate) primaries: i32,
        pub(crate) primaries_red_xy: [f64; 2],
        pub(crate) primaries_green_xy: [f64; 2],
        pub(crate) primaries_blue_xy: [f64; 2],
        pub(crate) transfer_function: i32,
        pub(crate) gamma: f64,
        pub(crate) rendering_intent: i32,
    }

    pub(crate) type JxlParallelRunner = unsafe extern "C" fn(
        runner_opaque: *mut c_void,
        jpegxl_opaque: *mut c_void,
        init: *const c_void,
        func: *const c_void,
        start_range: u32,
        end_range: u32,
    ) -> i32;

    #[link(name = "jxl")]
    unsafe extern "C" {
        pub(crate) fn JxlEncoderCreate(mm: *const JxlMemoryManager) -> *mut JxlEncoder;
        pub(crate) fn JxlEncoderDestroy(enc: *mut JxlEncoder);
        pub(crate) fn JxlEncoderSetParallelRunner(
            enc: *mut JxlEncoder,
            runner: JxlParallelRunner,
            opaque: *mut c_void,
        ) -> JxlEncoderStatus;
        pub(crate) fn JxlEncoderInitBasicInfo(info: *mut JxlBasicInfo);
        pub(crate) fn JxlEncoderSetBasicInfo(
            enc: *mut JxlEncoder,
            info: *const JxlBasicInfo,
        ) -> JxlEncoderStatus;
        pub(crate) fn JxlColorEncodingSetToSRGB(color: *mut JxlColorEncoding, is_gray: JXL_BOOL);
        pub(crate) fn JxlEncoderSetColorEncoding(
            enc: *mut JxlEncoder,
            color: *const JxlColorEncoding,
        ) -> JxlEncoderStatus;
        pub(crate) fn JxlEncoderSetICCProfile(
            enc: *mut JxlEncoder,
            icc: *const u8,
            size: usize,
        ) -> JxlEncoderStatus;
        pub(crate) fn JxlEncoderStoreJPEGMetadata(
            enc: *mut JxlEncoder,
            store: JXL_BOOL,
        ) -> JxlEncoderStatus;
        pub(crate) fn JxlEncoderFrameSettingsCreate(
            enc: *mut JxlEncoder,
            source: *const JxlEncoderFrameSettings,
        ) -> *mut JxlEncoderFrameSettings;
        pub(crate) fn JxlEncoderSetFrameLossless(
            settings: *mut JxlEncoderFrameSettings,
            lossless: JXL_BOOL,
        ) -> JxlEncoderStatus;
        pub(crate) fn JxlEncoderAddImageFrame(
            settings: *const JxlEncoderFrameSettings,
            format: *const JxlPixelFormat,
            buffer: *const c_void,
            size: usize,
        ) -> JxlEncoderStatus;
        pub(crate) fn JxlEncoderAddJPEGFrame(
            settings: *const JxlEncoderFrameSettings,
            buffer: *const u8,
            size: usize,
        ) -> JxlEncoderStatus;
        pub(crate) fn JxlEncoderCloseInput(enc: *mut JxlEncoder);
        pub(crate) fn JxlEncoderProcessOutput(
            enc: *mut JxlEncoder,
            next_out: *mut *mut u8,
            avail_out: *mut usize,
        ) -> JxlEncoderStatus;

        pub(crate) fn JxlDecoderCreate(mm: *const JxlMemoryManager) -> *mut JxlDecoder;
        pub(crate) fn JxlDecoderDestroy(dec: *mut JxlDecoder);
        pub(crate) fn JxlDecoderSubscribeEvents(
            dec: *mut JxlDecoder,
            events_wanted: i32,
        ) -> JxlDecoderStatus;
        pub(crate) fn JxlDecoderSetParallelRunner(
            dec: *mut JxlDecoder,
            runner: JxlParallelRunner,
            opaque: *mut c_void,
        ) -> JxlDecoderStatus;
        pub(crate) fn JxlDecoderSetInput(
            dec: *mut JxlDecoder,
            data: *const u8,
            size: usize,
        ) -> JxlDecoderStatus;
        pub(crate) fn JxlDecoderCloseInput(dec: *mut JxlDecoder);
        pub(crate) fn JxlDecoderProcessInput(dec: *mut JxlDecoder) -> JxlDecoderStatus;
        pub(crate) fn JxlDecoderGetBasicInfo(
            dec: *const JxlDecoder,
            info: *mut JxlBasicInfo,
        ) -> JxlDecoderStatus;
        pub(crate) fn JxlDecoderImageOutBufferSize(
            dec: *const JxlDecoder,
            format: *const JxlPixelFormat,
            size: *mut usize,
        ) -> JxlDecoderStatus;
        pub(crate) fn JxlDecoderSetImageOutBuffer(
            dec: *mut JxlDecoder,
            format: *const JxlPixelFormat,
            buffer: *mut c_void,
            size: usize,
        ) -> JxlDecoderStatus;
        pub(crate) fn JxlDecoderSetJPEGBuffer(
            dec: *mut JxlDecoder,
            data: *mut u8,
            size: usize,
        ) -> JxlDecoderStatus;
        pub(crate) fn JxlDecoderReleaseJPEGBuffer(dec: *mut JxlDecoder) -> usize;
    }

    #[link(name = "jxl_threads")]
    unsafe extern "C" {
        pub(crate) fn JxlThreadParallelRunner(
            runner_opaque: *mut c_void,
            jpegxl_opaque: *mut c_void,
            init: *const c_void,
            func: *const c_void,
            start_range: u32,
            end_range: u32,
        ) -> i32;
        pub(crate) fn JxlThreadParallelRunnerCreate(
            mm: *const JxlMemoryManager,
            num_worker_threads: usize,
        ) -> *mut c_void;
        pub(crate) fn JxlThreadParallelRunnerDestroy(runner: *mut c_void);
        pub(crate) fn JxlThreadParallelRunnerDefaultNumWorkerThreads() -> usize;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(width: u32, height: u32) -> Vec<u8> {
        let mut pixels = Vec::new();
        for y in 0..height {
            for x in 0..width {
                pixels.extend([x as u8, y as u8, (x ^ y) as u8, 200]);
            }
        }
        pixels
    }

    #[test]
    fn lossless_round_trip() {
        let pixels = gradient(37, 21);
        let layout = Layout {
            width: 37,
            height: 21,
            channels: 4,
            sixteen_bit: false,
        };
        let jxl = encode_lossless(&pixels, layout, None).unwrap();
        assert_eq!(dimensions(&jxl).unwrap(), (37, 21));
        let decoded = decode_rgba(&jxl).unwrap();
        assert_eq!((decoded.width, decoded.height), (37, 21));
        assert_eq!(decoded.pixels, pixels);
    }

    #[test]
    fn gray_sixteen_bit_decodes_to_rgba() {
        let samples: Vec<u16> = (0..12).map(|i| i * 5000).collect();
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_ne_bytes()).collect();
        let layout = Layout {
            width: 4,
            height: 3,
            channels: 1,
            sixteen_bit: true,
        };
        let jxl = encode_lossless(&bytes, layout, None).unwrap();
        let decoded = decode_rgba(&jxl).unwrap();
        // The second pixel, 5000/65535 gray; converting gray to RGB may
        // round differently per channel.
        let [r, g, b, a] = decoded.pixels[4..8] else {
            unreachable!()
        };
        for c in [r, g, b] {
            assert!((18..=21).contains(&c), "{:?}", &decoded.pixels[4..8]);
        }
        assert_eq!(a, 255);
    }

    #[test]
    fn recompressed_jpegs_are_reconstructed() {
        let jpeg = include_bytes!("test.jpg");
        let jxl = recompress_jpeg(jpeg).unwrap();
        assert_eq!(reconstruct_jpeg(&jxl).unwrap().as_deref(), Some(&jpeg[..]));
        // With a buffer that has to grow.
        let small = reconstruct_jpeg_into(16, &jxl).unwrap();
        assert_eq!(small.as_deref(), Some(&jpeg[..]));
    }

    #[test]
    fn only_recompressed_jpegs_are_reconstructed() {
        let layout = Layout {
            width: 5,
            height: 4,
            channels: 4,
            sixteen_bit: false,
        };
        let jxl = encode_lossless(&gradient(5, 4), layout, None).unwrap();
        assert_eq!(reconstruct_jpeg(&jxl).unwrap(), None);
        assert!(reconstruct_jpeg(b"not a jxl").is_err());
    }

    #[test]
    fn bad_input_is_an_error() {
        let layout = Layout {
            width: 2,
            height: 2,
            channels: 4,
            sixteen_bit: false,
        };
        assert!(encode_lossless(&[0; 3], layout, None).is_err());
        assert!(decode_rgba(b"not a jxl").is_err());
        assert!(dimensions(b"").is_err());
        assert!(recompress_jpeg(b"not a jpeg").is_err());
    }
}

// The README's examples, compiled as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
