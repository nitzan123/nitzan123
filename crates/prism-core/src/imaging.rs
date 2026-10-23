//! Decoding entry points. Everything that turns a file into pixels goes
//! through here so the rest of the crate never cares about the format.

use crate::error::{Error, Result};
use crate::metadata::read_exif;
use crate::raw::{decode_raw, extract_embedded_jpeg, is_raw_ext, LinearImage};
use image::{imageops, DynamicImage, RgbImage};
use std::path::Path;
use std::sync::OnceLock;

pub const RASTER_EXTENSIONS: &[&str] = &["jpg", "jpeg", "jpe", "jfif", "png", "tif", "tiff", "webp", "gif", "bmp"];

/// ISO-BMFF stills: iPhone/Samsung HEIC, generic HEIF, Canon/Sony HIF and
/// AVIF. The decoder applies the container's rotation/mirror itself.
pub const HEIF_EXTENSIONS: &[&str] = &["heic", "heif", "hif", "avif"];

/// JPEG XL. The decoder applies the codestream orientation itself.
pub const JXL_EXTENSIONS: &[&str] = &["jxl"];

pub fn ext_of(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

pub fn is_heif_ext(ext: &str) -> bool {
    HEIF_EXTENSIONS.contains(&ext)
}

fn is_raster_ext(ext: &str) -> bool {
    RASTER_EXTENSIONS.contains(&ext) || is_heif_ext(ext) || JXL_EXTENSIONS.contains(&ext)
}

pub fn is_supported(path: &Path) -> bool {
    let ext = ext_of(path);
    is_raster_ext(&ext) || is_raw_ext(&ext)
}

/// EXIF orientation still to apply after decoding. HEIF and JPEG XL decoders
/// already rotate (iPhones write both `irot` and EXIF orientation, so using
/// EXIF too would turn portraits sideways).
fn pending_orientation(path: &Path, ext: &str) -> u16 {
    if is_heif_ext(ext) || JXL_EXTENSIONS.contains(&ext) {
        1
    } else {
        read_exif(path).orientation
    }
}

/// Non-RAW decode. Content sniffing first, so a PNG saved as `.jpg` (common
/// with messaging apps) still opens.
fn decode_raster(path: &Path, ext: &str) -> Result<DynamicImage> {
    if is_heif_ext(ext) {
        return decode_heif(path);
    }
    if JXL_EXTENSIONS.contains(&ext) {
        return decode_jxl(path);
    }
    Ok(image::ImageReader::open(path)?.with_guessed_format()?.decode()?)
}

fn decode_error(path: &Path, reason: impl ToString) -> Error {
    Error::Decode { path: path.to_path_buf(), reason: reason.to_string() }
}

fn decode_heif(path: &Path) -> Result<DynamicImage> {
    let data = std::fs::read(path)?;
    let out = heic::DecoderConfig::new().decode(&data, heic::PixelLayout::Rgba8).map_err(|e| decode_error(path, e))?;
    let img = image::RgbaImage::from_raw(out.width, out.height, out.data)
        .ok_or_else(|| decode_error(path, "short pixel buffer"))?;
    Ok(DynamicImage::ImageRgba8(img))
}

fn decode_jxl(path: &Path) -> Result<DynamicImage> {
    let jxl = jxl_oxide::JxlImage::builder().open(path).map_err(|e| decode_error(path, e))?;
    let fb = jxl.render_frame(0).map_err(|e| decode_error(path, e))?.image_all_channels();
    let (w, h, ch) = (fb.width() as u32, fb.height() as u32, fb.channels());
    let rgb: Vec<f32> = fb
        .buf()
        .chunks_exact(ch)
        .flat_map(|px| match ch {
            1 | 2 => [px[0]; 3],
            _ => [px[0], px[1], px[2]],
        })
        .map(|v| v.clamp(0.0, 1.0))
        .collect();
    let img = image::Rgb32FImage::from_raw(w, h, rgb).ok_or_else(|| decode_error(path, "short pixel buffer"))?;
    Ok(DynamicImage::ImageRgb32F(img))
}

/// 8-bit sRGB image for browsing (thumbnails, viewer). RAW files use their
/// embedded preview, so this is fast.
pub fn load_display(path: &Path, max_dim: u32) -> Result<RgbImage> {
    let ext = ext_of(path);
    let img = if is_raw_ext(&ext) {
        let bytes = std::fs::read(path)?;
        match extract_embedded_jpeg(&bytes) {
            Some(jpeg) => image::load_from_memory(&jpeg)?,
            None => DynamicImage::ImageRgb32F(decode_raw(path, max_dim)?.map_to_srgb()),
        }
    } else if is_raster_ext(&ext) {
        decode_raster(path, &ext)?
    } else {
        return Err(Error::Unsupported(path.to_path_buf()));
    };
    let img = apply_orientation(img, pending_orientation(path, &ext));
    Ok(fit(img, max_dim).to_rgb8())
}

/// Linear-light float image for the develop pipeline. RAW files are decoded
/// from sensor data, not from the preview.
pub fn load_linear(path: &Path, max_dim: u32) -> Result<LinearImage> {
    let ext = ext_of(path);
    let lin = if is_raw_ext(&ext) {
        let orientation = read_exif(path).orientation;
        match decode_raw(path, max_dim) {
            Ok(lin) => {
                apply_orientation(DynamicImage::ImageRgb32F(downscale_linear(lin, max_dim)), orientation).to_rgb32f()
            }
            // Sensor data we can't decode (e.g. CR3): develop from the
            // embedded camera JPEG rather than refusing the file.
            Err(e) => {
                let bytes = std::fs::read(path)?;
                let jpeg = extract_embedded_jpeg(&bytes).ok_or(e)?;
                let img = apply_orientation(fit(image::load_from_memory(&jpeg)?, max_dim), orientation);
                to_linear(img.to_rgb32f())
            }
        }
    } else if is_raster_ext(&ext) {
        let img = apply_orientation(fit(decode_raster(path, &ext)?, max_dim), pending_orientation(path, &ext));
        to_linear(img.to_rgb32f())
    } else {
        return Err(Error::Unsupported(path.to_path_buf()));
    };
    Ok(lin)
}

fn to_linear(mut f: image::Rgb32FImage) -> LinearImage {
    let lut = srgb_to_linear_lut();
    for p in f.pixels_mut() {
        for c in 0..3 {
            p.0[c] = lut[(p.0[c].clamp(0.0, 1.0) * 4095.0).round() as usize];
        }
    }
    f
}

fn fit(img: DynamicImage, max_dim: u32) -> DynamicImage {
    if max_dim == 0 || (img.width() <= max_dim && img.height() <= max_dim) {
        img
    } else {
        img.resize(max_dim, max_dim, imageops::FilterType::Triangle)
    }
}

fn downscale_linear(img: LinearImage, max_dim: u32) -> LinearImage {
    if max_dim == 0 || (img.width() <= max_dim && img.height() <= max_dim) {
        return img;
    }
    let s = max_dim as f32 / img.width().max(img.height()) as f32;
    let w = ((img.width() as f32 * s).round() as u32).max(1);
    let h = ((img.height() as f32 * s).round() as u32).max(1);
    imageops::resize(&img, w, h, imageops::FilterType::Triangle)
}

pub fn apply_orientation(img: DynamicImage, orientation: u16) -> DynamicImage {
    match orientation {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate90().fliph(),
        6 => img.rotate90(),
        7 => img.rotate270().fliph(),
        8 => img.rotate270(),
        _ => img,
    }
}

#[inline]
pub fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

#[inline]
pub fn linear_to_srgb(v: f32) -> f32 {
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

pub fn srgb_to_linear_lut() -> &'static [f32; 4096] {
    static LUT: OnceLock<[f32; 4096]> = OnceLock::new();
    LUT.get_or_init(|| std::array::from_fn(|i| srgb_to_linear(i as f32 / 4095.0)))
}

trait MapToSrgb {
    fn map_to_srgb(self) -> image::Rgb32FImage;
}

impl MapToSrgb for LinearImage {
    fn map_to_srgb(mut self) -> image::Rgb32FImage {
        for p in self.pixels_mut() {
            for c in 0..3 {
                p.0[c] = linear_to_srgb(p.0[c].clamp(0.0, 1.0));
            }
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_roundtrip() {
        for i in 0..=20 {
            let v = i as f32 / 20.0;
            assert!((linear_to_srgb(srgb_to_linear(v)) - v).abs() < 1e-4);
        }
    }

    #[test]
    fn loads_and_fits_png() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.png");
        RgbImage::from_pixel(800, 400, image::Rgb([200, 100, 50])).save(&p).unwrap();
        let d = load_display(&p, 200).unwrap();
        assert_eq!((d.width(), d.height()), (200, 100));
        let l = load_linear(&p, 0).unwrap();
        assert_eq!((l.width(), l.height()), (800, 400));
        assert!((l.get_pixel(0, 0).0[0] - srgb_to_linear(200.0 / 255.0)).abs() < 2e-3);
    }
}
