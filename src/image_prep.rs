//! Making arbitrary photos look right on a Frame: EXIF rotation, fitting to the panel's
//! aspect ratio, sensible size, and a format the TV accepts (JPEG/PNG).

use std::io::Cursor;
use std::path::Path;
use std::str::FromStr;

use image::{DynamicImage, ImageFormat, ImageReader, imageops::FilterType};
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// File extensions picked up when scanning folders.
pub const SUPPORTED_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "bmp", "tif", "tiff", "gif"];

/// How to fit an image to the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum Fit {
    /// Fill the screen if the photo is already close to the panel's shape (within ~12%),
    /// otherwise keep the whole photo and let the TV's matte frame it.
    #[default]
    Auto,
    /// Always fill the screen, center-cropping as needed.
    Cover,
    /// Never crop; only scale down to fit within the panel.
    Contain,
    /// Upload untouched when the TV can take it as-is (JPEG/PNG, upright).
    Original,
}

impl FromStr for Fit {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Ok(Fit::Auto),
            "cover" | "fill" | "crop" => Ok(Fit::Cover),
            "contain" | "fit" => Ok(Fit::Contain),
            "original" | "none" => Ok(Fit::Original),
            other => Err(format!("unknown fit `{other}` (expected auto, cover, contain, original)")),
        }
    }
}

impl std::fmt::Display for Fit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Fit::Auto => "auto",
            Fit::Cover => "cover",
            Fit::Contain => "contain",
            Fit::Original => "original",
        })
    }
}

/// A region of the (upright) photo to keep, as fractions of its width and height.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct Crop {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Crop {
    /// The crop in pixels of a `w`x`h` image, or `None` if it keeps (nearly) everything.
    fn to_pixels(self, w: u32, h: u32) -> Option<(u32, u32, u32, u32)> {
        let clamp = |v: f32| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
        let x = ((clamp(self.x) * w as f32).round() as u32).min(w.saturating_sub(1));
        let y = ((clamp(self.y) * h as f32).round() as u32).min(h.saturating_sub(1));
        let cw = ((clamp(self.w) * w as f32).round() as u32).clamp(1, w - x);
        let ch = ((clamp(self.h) * h as f32).round() as u32).clamp(1, h - y);
        ((x, y, cw, ch) != (0, 0, w, h)).then_some((x, y, cw, ch))
    }
}

impl FromStr for Crop {
    type Err = String;
    /// `x,y,w,h` fractions, e.g. `0.1,0,0.8,1`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let v: Vec<f32> = s
            .split(',')
            .map(|p| p.trim().parse::<f32>())
            .collect::<Result<_, _>>()
            .map_err(|e| format!("bad crop `{s}`: {e}"))?;
        match v[..] {
            [x, y, w, h] if v.iter().all(|n| n.is_finite()) && w > 0.0 && h > 0.0 => Ok(Crop { x, y, w, h }),
            _ => Err(format!("bad crop `{s}` (expected x,y,w,h fractions)")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PrepOptions {
    pub fit: Fit,
    /// Keep only this part of the photo (applied before fitting).
    pub crop: Option<Crop>,
    /// Panel size; 3840x2160 for 43"+ Frames, 1920x1080 for the 32".
    pub width: u32,
    pub height: u32,
    pub jpeg_quality: u8,
    /// How far (as a fraction) the photo's aspect may differ from the panel's for `Auto` to crop.
    pub auto_crop_tolerance: f32,
}

impl Default for PrepOptions {
    fn default() -> Self {
        Self { fit: Fit::Auto, crop: None, width: 3840, height: 2160, jpeg_quality: 90, auto_crop_tolerance: 0.12 }
    }
}

#[derive(Debug, Clone)]
pub struct Prepared {
    pub data: Vec<u8>,
    /// `jpg` or `png`.
    pub file_type: &'static str,
    pub width: u32,
    pub height: u32,
    /// From EXIF `DateTimeOriginal`, as `YYYY:MM:DD HH:MM:SS`.
    pub image_date: Option<String>,
    /// False when the original bytes are uploaded unchanged.
    pub reencoded: bool,
    pub cropped: bool,
}

impl Prepared {
    /// Whether the image fills the panel exactly (so a matte is unnecessary).
    pub fn fills(&self, width: u32, height: u32) -> bool {
        let a = self.width as f64 / self.height as f64;
        let b = width as f64 / height as f64;
        (a / b - 1.0).abs() < 0.01
    }
}

#[derive(Default)]
struct ExifBits {
    orientation: u32,
    date: Option<String>,
    /// PixelXDimension/PixelYDimension, as stored.
    pixel_dims: Option<(u32, u32)>,
}

fn read_exif(bytes: &[u8]) -> ExifBits {
    let Ok(exif) = exif::Reader::new().read_from_container(&mut Cursor::new(bytes)) else {
        return ExifBits { orientation: 1, ..Default::default() };
    };
    let orientation =
        exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY).and_then(|f| f.value.get_uint(0)).unwrap_or(1);
    let date = [exif::Tag::DateTimeOriginal, exif::Tag::DateTime].iter().find_map(|tag| {
        let field = exif.get_field(*tag, exif::In::PRIMARY)?;
        match &field.value {
            exif::Value::Ascii(v) => {
                let s = String::from_utf8_lossy(v.first()?).trim().to_string();
                // `YYYY:MM:DD HH:MM:SS`, which is exactly what the TV wants.
                (s.len() == 19 && !s.starts_with("0000")).then_some(s)
            }
            _ => None,
        }
    });
    let dim = |tag| exif.get_field(tag, exif::In::PRIMARY).and_then(|f| f.value.get_uint(0));
    let pixel_dims = dim(exif::Tag::PixelXDimension).zip(dim(exif::Tag::PixelYDimension));
    ExifBits { orientation, date, pixel_dims }
}

fn is_heif(bytes: &[u8]) -> bool {
    bytes.len() > 12
        && &bytes[4..8] == b"ftyp"
        && matches!(&bytes[8..12], b"heic" | b"heix" | b"mif1" | b"msf1" | b"heif" | b"avif")
}

/// The largest box with the panel's aspect ratio that fits inside `w`x`h`, capped at the
/// panel size (never upscale; the TV scales smaller images itself).
fn cover_box(w: u32, h: u32, pw: u32, ph: u32) -> (u32, u32) {
    if w >= pw && h >= ph {
        return (pw, ph);
    }
    let target = pw as f64 / ph as f64;
    if (w as f64 / h as f64) > target {
        (((h as f64) * target).round() as u32, h)
    } else {
        (w, ((w as f64) / target).round() as u32)
    }
}

/// Decode a HEIF image and make it upright.
fn decode_heif_upright(bytes: &[u8], exif: &ExifBits) -> Result<DynamicImage> {
    let mut img = heif::decode(bytes)?;
    // The platform decoder may already apply the HEIF rotation; only rotate if the pixels
    // still have the stored (pre-rotation) shape.
    let rotated_already = matches!(exif.orientation, 5..=8)
        && exif.pixel_dims.is_some_and(|(x, y)| x != y && (img.width(), img.height()) == (y, x));
    if !rotated_already
        && exif.orientation != 1
        && let Some(o) = image::metadata::Orientation::from_exif(exif.orientation as u8)
    {
        img.apply_orientation(o);
    }
    Ok(img)
}

/// Decode any supported image (HEIF included), applying its EXIF rotation.
fn decode_upright(bytes: &[u8]) -> Result<DynamicImage> {
    let exif = read_exif(bytes);
    match image::guess_format(bytes) {
        Ok(format) => {
            let mut img = image::load_from_memory_with_format(bytes, format)?;
            if let Some(o) = image::metadata::Orientation::from_exif(exif.orientation as u8) {
                img.apply_orientation(o);
            }
            Ok(img)
        }
        Err(_) if is_heif(bytes) => decode_heif_upright(bytes, &exif),
        Err(_) => Err(Error::Other("unrecognized image format".into())),
    }
}

/// An upright JPEG no larger than `max` pixels on either side, for showing in a browser
/// (which may not decode HEIC itself).
pub fn preview(bytes: &[u8], max: u32) -> Result<Vec<u8>> {
    let img = decode_upright(bytes)?;
    let img = if img.width() > max || img.height() > max { img.resize(max, max, FilterType::Triangle) } else { img };
    encode_jpeg(&img, 85)
}

pub fn prepare(bytes: &[u8], opts: &PrepOptions) -> Result<Prepared> {
    let mut exif = read_exif(bytes);
    let mut decoded: Option<DynamicImage> = None;
    let format = match image::guess_format(bytes) {
        Ok(f) => Some(f),
        Err(_) if is_heif(bytes) => {
            decoded = Some(decode_heif_upright(bytes, &exif)?);
            exif.orientation = 1;
            None
        }
        Err(_) => return Err(Error::Other("unrecognized image format".into())),
    };
    let upright = exif.orientation == 1;
    let tv_native = matches!(format, Some(ImageFormat::Jpeg | ImageFormat::Png));
    let file_type = if format == Some(ImageFormat::Png) { "png" } else { "jpg" };

    // Dimensions without decoding, after accounting for 90/270 degree EXIF rotations.
    let (full_w, full_h) = match (&decoded, format) {
        (Some(img), _) => (img.width(), img.height()),
        (None, Some(format)) => {
            let (raw_w, raw_h) = ImageReader::with_format(Cursor::new(bytes), format).into_dimensions()?;
            if matches!(exif.orientation, 5..=8) { (raw_h, raw_w) } else { (raw_w, raw_h) }
        }
        (None, None) => unreachable!("either decoded or a known format"),
    };
    let crop = opts.crop.and_then(|c| c.to_pixels(full_w, full_h));
    let (w, h) = crop.map_or((full_w, full_h), |(_, _, cw, ch)| (cw, ch));
    let (pw, ph) = (opts.width, opts.height);

    let fit = match opts.fit {
        Fit::Auto => {
            let off = ((w as f32 / h as f32) / (pw as f32 / ph as f32) - 1.0).abs();
            if off <= opts.auto_crop_tolerance { Fit::Cover } else { Fit::Contain }
        }
        f => f,
    };

    // Target size for the chosen fit.
    let target = match fit {
        Fit::Cover => cover_box(w, h, pw, ph),
        Fit::Contain if w > pw || h > ph => {
            let scale = (pw as f64 / w as f64).min(ph as f64 / h as f64);
            (((w as f64) * scale).round().max(1.0) as u32, ((h as f64) * scale).round().max(1.0) as u32)
        }
        _ => (w, h),
    };

    let pass_through = crop.is_none() && tv_native && upright && (fit == Fit::Original || target == (w, h));
    if pass_through {
        return Ok(Prepared {
            data: bytes.to_vec(),
            file_type,
            width: w,
            height: h,
            image_date: exif.date,
            reencoded: false,
            cropped: false,
        });
    }

    let img = match (decoded, format) {
        (Some(img), _) => img,
        (None, Some(format)) => {
            let mut img = image::load_from_memory_with_format(bytes, format)?;
            if !upright && let Some(o) = image::metadata::Orientation::from_exif(exif.orientation as u8) {
                img.apply_orientation(o);
            }
            img
        }
        (None, None) => unreachable!("either decoded or a known format"),
    };
    let img = match crop {
        Some((x, y, cw, ch)) => img.crop_imm(x, y, cw, ch),
        None => img,
    };
    let cropped = crop.is_some() || (fit == Fit::Cover && (w as u64 * target.1 as u64 != h as u64 * target.0 as u64));
    let img = match fit {
        Fit::Cover if (img.width(), img.height()) != target => {
            img.resize_to_fill(target.0, target.1, FilterType::Lanczos3)
        }
        Fit::Contain if (img.width(), img.height()) != target => {
            img.resize_exact(target.0, target.1, FilterType::Lanczos3)
        }
        _ => img,
    };
    Ok(Prepared {
        width: img.width(),
        height: img.height(),
        data: encode_jpeg(&img, opts.jpeg_quality)?,
        file_type: "jpg",
        image_date: exif.date,
        reencoded: true,
        cropped,
    })
}

fn encode_jpeg(img: &DynamicImage, quality: u8) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality.clamp(1, 100));
    img.to_rgb8().write_with_encoder(encoder)?;
    Ok(out)
}

/// Read and prepare a file; falls back to the file's modification time for the image date.
pub fn prepare_file(path: &Path, opts: &PrepOptions) -> Result<Prepared> {
    let bytes = std::fs::read(path)?;
    let mut prepared = prepare(&bytes, opts).map_err(|e| Error::Other(format!("{}: {e}", path.display())))?;
    if prepared.image_date.is_none()
        && let Ok(modified) = std::fs::metadata(path).and_then(|m| m.modified())
    {
        let local: chrono::DateTime<chrono::Local> = modified.into();
        prepared.image_date = Some(local.format("%Y:%m:%d %H:%M:%S").to_string());
    }
    Ok(prepared)
}

/// HEIC/HEIF/AVIF (iPhone photos). Decodable where the platform has a codec (Windows with the
/// HEIF Image Extensions).
pub const HEIF_EXTENSIONS: &[&str] = &["heic", "heif", "hif", "avif"];

/// Whether this machine can decode HEIC/HEIF.
pub fn heif_supported() -> bool {
    heif::AVAILABLE
}

/// Supported extensions, plus HEIF ones when [`heif_supported`].
pub fn is_supported_or_heif_path(path: &Path) -> bool {
    is_supported_path(path)
        || (heif_supported()
            && path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| HEIF_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str())))
}

pub fn is_supported_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| SUPPORTED_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

#[cfg(windows)]
mod heif {
    //! HEIF decoding through Windows Imaging Component (needs the HEIF Image Extensions and
    //! HEVC Video Extensions from the Microsoft Store, preinstalled on most Windows 11 PCs).
    use image::{DynamicImage, RgbaImage};
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_WICPixelFormat32bppRGBA, IWICImagingFactory, WICBitmapDitherTypeNone,
        WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
    };
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};

    use crate::{Error, Result};

    pub const AVAILABLE: bool = true;

    pub fn decode(bytes: &[u8]) -> Result<DynamicImage> {
        decode_wic(bytes).map_err(|e| {
            Error::Other(format!(
                "could not decode HEIC/HEIF ({e}); install \"HEIF Image Extensions\" and \"HEVC Video Extensions\" from the Microsoft Store"
            ))
        })
    }

    fn decode_wic(bytes: &[u8]) -> windows::core::Result<DynamicImage> {
        unsafe {
            // Fine if COM is already initialized on this thread (in any mode).
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            let stream = factory.CreateStream()?;
            stream.InitializeFromMemory(bytes)?;
            let decoder = factory.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)?;
            let frame = decoder.GetFrame(0)?;
            let converter = factory.CreateFormatConverter()?;
            converter.Initialize(
                &frame,
                &GUID_WICPixelFormat32bppRGBA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeCustom,
            )?;
            let (mut w, mut h) = (0u32, 0u32);
            converter.GetSize(&raw mut w, &raw mut h)?;
            let mut buf = vec![0u8; w as usize * h as usize * 4];
            converter.CopyPixels(std::ptr::null(), w * 4, &mut buf)?;
            let img = RgbaImage::from_raw(w, h, buf)
                .ok_or_else(|| windows::core::Error::from_hresult(windows::core::HRESULT(-1)))?;
            Ok(DynamicImage::ImageRgba8(img))
        }
    }
}

#[cfg(not(windows))]
mod heif {
    //! HEIF decoding through the system libheif (>= 1.17, plus its libde265 plugin).
    use image::{DynamicImage, RgbImage};
    use libheif_rs::{ColorSpace, HeifContext, LibHeif, RgbChroma};

    use crate::{Error, Result};

    pub const AVAILABLE: bool = true;

    pub fn decode(bytes: &[u8]) -> Result<DynamicImage> {
        let fail = |e: libheif_rs::HeifError| Error::Other(format!("could not decode HEIC/HEIF: {e}"));
        let ctx = HeifContext::read_from_bytes(bytes).map_err(fail)?;
        let handle = ctx.primary_image_handle().map_err(fail)?;
        // libheif applies the container's rotation/mirroring, so the result is upright.
        let image = LibHeif::new().decode(&handle, ColorSpace::Rgb(RgbChroma::Rgb), None).map_err(fail)?;
        let plane = image.planes().interleaved.ok_or_else(|| Error::Other("HEIF decoder returned no pixels".into()))?;
        let (w, h, stride) = (plane.width, plane.height, plane.stride);
        let mut buf = Vec::with_capacity(w as usize * h as usize * 3);
        for row in plane.data.chunks(stride).take(h as usize) {
            buf.extend_from_slice(&row[..w as usize * 3]);
        }
        let img = RgbImage::from_raw(w, h, buf).ok_or_else(|| Error::Other("bad HEIF image size".into()))?;
        Ok(DynamicImage::ImageRgb8(img))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb};

    fn jpeg(w: u32, h: u32) -> Vec<u8> {
        let img = DynamicImage::ImageRgb8(ImageBuffer::from_pixel(w, h, Rgb([200u8, 100, 50])));
        encode_jpeg(&img, 80).unwrap()
    }

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = DynamicImage::ImageRgb8(ImageBuffer::from_pixel(w, h, Rgb([1u8, 2, 3])));
        let mut out = Vec::new();
        img.write_to(&mut Cursor::new(&mut out), ImageFormat::Png).unwrap();
        out
    }

    fn small_panel() -> PrepOptions {
        PrepOptions { width: 160, height: 90, ..Default::default() }
    }

    #[test]
    fn exact_fit_passes_through_untouched() {
        let src = jpeg(160, 90);
        let p = prepare(&src, &small_panel()).unwrap();
        assert!(!p.reencoded);
        assert_eq!(p.data, src);
    }

    #[test]
    fn near_16x9_is_cropped_to_fill() {
        // 16:10 is within tolerance of 16:9 -> cover.
        let p = prepare(&jpeg(320, 200), &small_panel()).unwrap();
        assert!(p.reencoded && p.cropped);
        assert_eq!((p.width, p.height), (160, 90));
    }

    #[test]
    fn portrait_is_kept_whole_and_scaled_down() {
        let p = prepare(&jpeg(300, 400), &small_panel()).unwrap();
        assert!(!p.cropped);
        assert_eq!((p.width, p.height), (68, 90));
        assert!(!p.fills(160, 90));
    }

    #[test]
    fn small_images_are_not_upscaled() {
        let p = prepare(&jpeg(80, 50), &PrepOptions { fit: Fit::Cover, ..small_panel() }).unwrap();
        assert_eq!((p.width, p.height), (80, 45));
    }

    #[test]
    fn original_keeps_png() {
        let src = png(1000, 10);
        let p = prepare(&src, &PrepOptions { fit: Fit::Original, ..small_panel() }).unwrap();
        assert_eq!(p.file_type, "png");
        assert!(!p.reencoded);
    }

    #[test]
    fn non_native_formats_are_converted() {
        let img = DynamicImage::ImageRgb8(ImageBuffer::from_pixel(16, 9, Rgb([9u8, 9, 9])));
        let mut bmp = Vec::new();
        img.write_to(&mut Cursor::new(&mut bmp), ImageFormat::Bmp).unwrap();
        let p = prepare(&bmp, &PrepOptions { fit: Fit::Original, ..small_panel() }).unwrap();
        assert_eq!(p.file_type, "jpg");
        assert!(p.reencoded);
    }

    #[test]
    fn crop_keeps_the_chosen_region() {
        // Right half white: a 16:9 slice from the right half fills the panel with white.
        let mut img = image::RgbImage::from_pixel(400, 200, Rgb([0, 0, 0]));
        for y in 0..200 {
            for x in 200..400 {
                img.put_pixel(x, y, Rgb([255, 255, 255]));
            }
        }
        let src = encode_jpeg(&DynamicImage::ImageRgb8(img), 95).unwrap();
        let crop = "0.5,0.2,0.4,0.45".parse::<Crop>().unwrap();
        let p = prepare(&src, &PrepOptions { fit: Fit::Cover, crop: Some(crop), ..small_panel() }).unwrap();
        assert!(p.reencoded && p.cropped);
        assert_eq!((p.width, p.height), (160, 90));
        let out = image::load_from_memory(&p.data).unwrap().to_rgb8();
        assert!(out.get_pixel(80, 45)[0] > 200, "crop came from the white half");
        // A crop that keeps everything is ignored, so an exact fit still passes through.
        let whole = Crop { x: 0.0, y: 0.0, w: 1.0, h: 1.0 };
        let exact = jpeg(160, 90);
        assert!(!prepare(&exact, &PrepOptions { crop: Some(whole), ..small_panel() }).unwrap().reencoded);
        assert!("1,2,3".parse::<Crop>().is_err());
    }

    #[test]
    fn preview_is_small_jpeg() {
        let p = preview(&png(1000, 500), 100).unwrap();
        let img = image::load_from_memory(&p).unwrap();
        assert_eq!((img.width(), img.height()), (100, 50));
    }

    #[test]
    fn heic_gets_helpful_error() {
        let mut fake = vec![0, 0, 0, 24];
        fake.extend_from_slice(b"ftypheic");
        fake.extend_from_slice(&[0; 16]);
        let err = prepare(&fake, &small_panel()).unwrap_err().to_string();
        assert!(err.contains("HEI"), "{err}");
    }

    /// Decodes a real HEIC when `FRAMETV_TEST_HEIC` names one (e.g. made with `heif-enc`);
    /// skipped otherwise. The file should be 320x180 or similar 16:9.
    #[test]
    fn decodes_real_heic_when_provided() {
        let Some(path) = std::env::var_os("FRAMETV_TEST_HEIC") else { return };
        assert!(heif_supported(), "FRAMETV_TEST_HEIC set but HEIF support is not compiled in");
        let p = prepare_file(Path::new(&path), &small_panel()).unwrap();
        assert_eq!((p.width, p.height), (160, 90));
        assert_eq!(p.file_type, "jpg");
        assert!(image::load_from_memory(&p.data).is_ok());
    }
}
