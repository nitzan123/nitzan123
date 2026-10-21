//! Camera RAW support.
//!
//! * `extract_embedded_jpeg` — fast path for thumbnails/browsing: nearly all
//!   RAW files carry a full-size JPEG preview.
//! * `decode_raw` — real decode for Develop: sensor data → black/white level
//!   normalisation → 2×2 superpixel demosaic → camera white balance →
//!   camera-to-linear-sRGB matrix. Output is half resolution, linear light.

use crate::error::{Error, Result};
use image::{ImageBuffer, Rgb};
use std::path::Path;

pub type LinearImage = ImageBuffer<Rgb<f32>, Vec<f32>>;

pub const RAW_EXTENSIONS: &[&str] = &[
    "cr2", "cr3", "crw", "nef", "nrw", "arw", "srf", "sr2", "dng", "orf", "rw2", "raf", "pef", "srw", "erf", "kdc",
    "dcr", "mrw", "3fr", "mef", "mos", "iiq", "x3f", "rwl",
];

pub fn is_raw_ext(ext: &str) -> bool {
    RAW_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
}

/// Returns the largest complete JPEG stream embedded in `bytes`.
pub fn extract_embedded_jpeg(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut best: Option<(usize, usize)> = None;
    let mut i = 0;
    while i + 3 < bytes.len() {
        // SOI followed by a marker (APPn / DQT / SOF …).
        if bytes[i] == 0xFF && bytes[i + 1] == 0xD8 && bytes[i + 2] == 0xFF {
            if let Some(end) = find_jpeg_end(bytes, i + 2) {
                let len = end - i;
                if len > 4096 && best.is_none_or(|(_, l)| len > l) {
                    best = Some((i, len));
                }
                i = end;
                continue;
            }
        }
        i += 1;
    }
    best.map(|(start, len)| bytes[start..start + len].to_vec())
}

/// Walks JPEG segments starting at `pos` (pointing at a 0xFF marker) and
/// returns the byte index just past EOI.
fn find_jpeg_end(b: &[u8], mut pos: usize) -> Option<usize> {
    while pos + 3 < b.len() {
        if b[pos] != 0xFF {
            return None;
        }
        let marker = b[pos + 1];
        match marker {
            0xD9 => return Some(pos + 2),
            0x01 | 0xD0..=0xD7 => pos += 2,
            0xDA => {
                // Start of scan: skip header, then entropy data until a
                // non-RST marker.
                let seg = u16::from_be_bytes([b[pos + 2], b[pos + 3]]) as usize;
                pos += 2 + seg;
                while pos + 1 < b.len() {
                    if b[pos] == 0xFF && b[pos + 1] != 0x00 && !(0xD0..=0xD7).contains(&b[pos + 1]) {
                        break;
                    }
                    pos += 1;
                }
            }
            0xFF => pos += 1,
            _ => {
                let seg = u16::from_be_bytes([b[pos + 2], b[pos + 3]]) as usize;
                if seg < 2 {
                    return None;
                }
                pos += 2 + seg;
            }
        }
    }
    None
}

/// sRGB (D65) → XYZ.
const SRGB_TO_XYZ: [[f32; 3]; 3] =
    [[0.412_453, 0.357_580, 0.180_423], [0.212_671, 0.715_160, 0.072_169], [0.019_334, 0.119_193, 0.950_227]];

pub fn decode_raw(path: &Path) -> Result<LinearImage> {
    let err = |reason: String| Error::Raw { path: path.to_path_buf(), reason };
    let raw = rawloader::decode_file(path).map_err(|e| err(e.to_string()))?;
    if raw.cpp != 1 {
        return Err(err(format!("unsupported {} components per pixel", raw.cpp)));
    }
    let (w, h) = (raw.width, raw.height);
    let data: Vec<f32> = match &raw.data {
        rawloader::RawImageData::Integer(v) => v.iter().map(|&x| x as f32).collect(),
        rawloader::RawImageData::Float(v) => v.clone(),
    };

    let black = raw.blacklevels.map(|x| x as f32);
    let white = raw.whitelevels.map(|x| x as f32);

    // White balance normalised to green.
    let mut wb = raw.wb_coeffs;
    if !wb.iter().take(3).all(|x| x.is_finite() && *x > 0.0) {
        wb = [1.0; 4];
    }
    let g = wb[1].max(1e-6);
    let wb = [wb[0] / g, 1.0, wb[2] / g];

    let rgb_cam = camera_to_srgb(&raw.xyz_to_cam);

    // Superpixel cell: 2×2 for Bayer, 3×3 for Fuji X-Trans (6×6 pattern,
    // every aligned 3×3 block holds R, G and B). Respect the crops
    // [top, right, bottom, left] and align cells to the pattern.
    let cell = if raw.cfa.width == 6 && raw.cfa.height == 6 { 3 } else { 2 };
    let [ct, cr, cb, cl] = raw.crops;
    let x0 = cl.div_ceil(cell) * cell;
    let y0 = ct.div_ceil(cell) * cell;
    let x1 = w.saturating_sub(cr);
    let y1 = h.saturating_sub(cb);
    let ow = (x1.saturating_sub(x0)) / cell;
    let oh = (y1.saturating_sub(y0)) / cell;
    if ow == 0 || oh == 0 {
        return Err(err("empty image after crop".into()));
    }

    let mut out = vec![0f32; ow * oh * 3];
    use rayon::prelude::*;
    out.par_chunks_mut(ow * 3).enumerate().for_each(|(oy, row)| {
        for ox in 0..ow {
            let mut acc = [0f32; 3];
            let mut n = [0f32; 3];
            for dy in 0..cell {
                for dx in 0..cell {
                    let y = y0 + oy * cell + dy;
                    let x = x0 + ox * cell + dx;
                    let c = raw.cfa.color_at(y, x).min(3);
                    let ch = if c == 3 { 1 } else { c };
                    let v = (data[y * w + x] - black[c]) / (white[c] - black[c]).max(1.0);
                    acc[ch] += v.max(0.0);
                    n[ch] += 1.0;
                }
            }
            // A channel absent from the cell (unusual CFA) borrows green so
            // it never turns into a coloured dot.
            let g = if n[1] > 0.0 { acc[1] / n[1] } else { 0.0 };
            let mean = |c: usize| if n[c] > 0.0 { acc[c] / n[c] } else { g };
            let cam = [mean(0) * wb[0], mean(1) * wb[1], mean(2) * wb[2]];
            for c in 0..3 {
                row[ox * 3 + c] = (rgb_cam[c][0] * cam[0] + rgb_cam[c][1] * cam[1] + rgb_cam[c][2] * cam[2]).max(0.0);
            }
        }
    });

    apply_base_curve(&mut out);
    LinearImage::from_raw(ow as u32, oh as u32, out).ok_or_else(|| err("buffer size mismatch".into()))
}

/// Baseline exposure (stops) and S-curve strength applied to every RAW so
/// an unedited RAW looks like a typical camera rendering instead of flat
/// scene-linear data. Tuned by eye against in-camera JPEG previews.
const BASE_EXPOSURE_EV: f32 = 0.25;
const BASE_CONTRAST: f32 = 0.3;

fn apply_base_curve(px: &mut [f32]) {
    use rayon::prelude::*;
    let gain = 2f32.powf(BASE_EXPOSURE_EV);
    px.par_chunks_mut(3).for_each(|p| {
        let y = 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
        if y <= 1e-7 {
            return;
        }
        let v = (y * gain).powf(1.0 / 2.2);
        let vc = v.min(1.0);
        let s = vc * vc * (3.0 - 2.0 * vc);
        let nv = v + BASE_CONTRAST * (s - vc);
        let k = nv.max(0.0).powf(2.2) / y;
        p.iter_mut().for_each(|c| *c *= k);
    });
}

/// dcraw-style camera→sRGB matrix from the XYZ→camera matrix.
fn camera_to_srgb(xyz_to_cam: &[[f32; 3]; 4]) -> [[f32; 3]; 3] {
    let identity = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    if xyz_to_cam.iter().take(3).all(|r| r.iter().all(|v| *v == 0.0)) {
        return identity;
    }
    // cam_rgb = xyz_to_cam * srgb_to_xyz, rows normalised to sum 1.
    let mut cam_rgb = [[0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            cam_rgb[i][j] = (0..3).map(|k| xyz_to_cam[i][k] * SRGB_TO_XYZ[k][j]).sum();
        }
        let s: f32 = cam_rgb[i].iter().sum();
        if s.abs() > 1e-6 {
            for v in &mut cam_rgb[i] {
                *v /= s;
            }
        }
    }
    invert3(&cam_rgb).unwrap_or(identity)
}

fn invert3(m: &[[f32; 3]; 3]) -> Option<[[f32; 3]; 3]> {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if det.abs() < 1e-9 {
        return None;
    }
    let d = 1.0 / det;
    Some([
        [
            (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * d,
            (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * d,
            (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * d,
        ],
        [
            (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * d,
            (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * d,
            (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * d,
        ],
        [
            (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * d,
            (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * d,
            (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * d,
        ],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_jpeg(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([(x * 7) as u8, (y * 5) as u8, 90]));
        let mut buf = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 90).encode_image(&img).unwrap();
        buf
    }

    #[test]
    fn finds_largest_embedded_jpeg() {
        let small = tiny_jpeg(64, 64);
        let big = tiny_jpeg(256, 200);
        let mut container = vec![0u8; 100];
        container.extend_from_slice(&small);
        container.extend_from_slice(&[1, 2, 3, 0xFF, 0x00]);
        container.extend_from_slice(&big);
        container.extend_from_slice(&[9; 50]);
        let found = extract_embedded_jpeg(&container).expect("jpeg");
        assert_eq!(found, big);
        let img = image::load_from_memory(&found).unwrap();
        assert_eq!((img.width(), img.height()), (256, 200));
    }

    #[test]
    fn base_curve_lifts_mids_and_keeps_black() {
        let mut px = vec![0.0, 0.0, 0.0, 0.18, 0.18, 0.18, 0.002, 0.002, 0.002];
        apply_base_curve(&mut px);
        assert_eq!(px[0], 0.0);
        assert!(px[3] > 0.18, "mid grey {}", px[3]);
        assert!(px[6] < 0.002 * 2f32.powf(BASE_EXPOSURE_EV), "deep shadow should get contrast");
    }

    #[test]
    fn identity_matrix_inverts() {
        let m = [[2.0, 0.0, 0.0], [0.0, 4.0, 0.0], [0.0, 0.0, 0.5]];
        let inv = invert3(&m).unwrap();
        assert!((inv[0][0] - 0.5).abs() < 1e-6 && (inv[2][2] - 2.0).abs() < 1e-6);
    }

    #[test]
    fn raw_extension_detection() {
        assert!(is_raw_ext("NEF"));
        assert!(!is_raw_ext("jpg"));
    }
}
