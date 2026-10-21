//! Decoding entry points. Everything that turns a file into pixels goes
//! through here so the rest of the crate never cares about the format.

use crate::error::{Error, Result};
use crate::metadata::read_exif;
use crate::raw::{decode_raw, extract_embedded_jpeg, is_raw_ext, LinearImage};
use image::{imageops, DynamicImage, RgbImage};
use std::path::Path;
use std::sync::OnceLock;

pub const RASTER_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "tif", "tiff", "webp", "gif", "bmp"];

pub fn ext_of(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

pub fn is_supported(path: &Path) -> bool {
    let ext = ext_of(path);
    RASTER_EXTENSIONS.contains(&ext.as_str()) || is_raw_ext(&ext)
}

/// 8-bit sRGB image for browsing (thumbnails, viewer). RAW files use their
/// embedded preview, so this is fast.
pub fn load_display(path: &Path, max_dim: u32) -> Result<RgbImage> {
    let ext = ext_of(path);
    let img = if is_raw_ext(&ext) {
        let bytes = std::fs::read(path)?;
        match extract_embedded_jpeg(&bytes) {
            Some(jpeg) => image::load_from_memory(&jpeg)?,
            None => DynamicImage::ImageRgb32F(decode_raw(path)?.map_to_srgb()),
        }
    } else if RASTER_EXTENSIONS.contains(&ext.as_str()) {
        image::open(path)?
    } else {
        return Err(Error::Unsupported(path.to_path_buf()));
    };
    let img = apply_orientation(img, read_exif(path).orientation);
    Ok(fit(img, max_dim).to_rgb8())
}

/// Linear-light float image for the develop pipeline. RAW files are decoded
/// from sensor data, not from the preview.
pub fn load_linear(path: &Path, max_dim: u32) -> Result<LinearImage> {
    let ext = ext_of(path);
    let orientation = read_exif(path).orientation;
    let lin = if is_raw_ext(&ext) {
        let lin = decode_raw(path)?;
        let lin = downscale_linear(lin, max_dim);
        let dynimg = apply_orientation(DynamicImage::ImageRgb32F(lin), orientation);
        dynimg.to_rgb32f()
    } else if RASTER_EXTENSIONS.contains(&ext.as_str()) {
        let img = apply_orientation(fit(image::open(path)?, max_dim), orientation);
        let mut f = img.to_rgb32f();
        let lut = srgb_to_linear_lut();
        for p in f.pixels_mut() {
            for c in 0..3 {
                p.0[c] = lut[(p.0[c].clamp(0.0, 1.0) * 4095.0).round() as usize];
            }
        }
        f
    } else {
        return Err(Error::Unsupported(path.to_path_buf()));
    };
    Ok(lin)
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
