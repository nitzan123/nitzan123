//! Mask providers for local adjustments. A mask is a per-pixel weight in
//! 0…1 computed on the (geometry-transformed) image. Providers only decide
//! *where*; the adjustment engine is shared. New providers (e.g. an ML
//! subject/sky segmenter) plug in here without touching the pipeline.

use super::buffer::{blur_plane, luma, rgb_to_hsl, smoothstep, Buf};
use image::ImageEncoder;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Mask {
    /// Whole image.
    #[default]
    Full,
    /// Full strength at (x0, y0), fading to zero at (x1, y1). Normalised.
    Linear { x0: f32, y0: f32, x1: f32, y1: f32 },
    /// Ellipse centred at (cx, cy) with radii (rx, ry), normalised to the
    /// image; `feather` 0…100 is the soft-edge width.
    Radial { cx: f32, cy: f32, rx: f32, ry: f32, feather: f32 },
    /// Heuristic sky detection (colour + brightness + connection to the top
    /// edge). `sensitivity` 0…100.
    Sky { sensitivity: f32 },
    /// A precomputed (e.g. AI) mask over the whole uncropped photo, stored
    /// as a base64 grayscale PNG. The pipeline runs it through the same lens
    /// and geometry transforms as the image so it stays aligned.
    Bitmap { label: String, width: u32, height: u32, data: String },
    /// Painted by hand: strokes applied in order (paint adds, erase removes).
    Brush { strokes: Vec<Stroke> },
}

/// One brush stroke. Points are normalised to the image like the other
/// masks; `size` is the brush radius as a fraction of the long edge.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Stroke {
    pub erase: bool,
    pub size: f32,
    /// 0 (hard edge) … 100 (fully soft).
    pub feather: f32,
    /// Strength 0…100.
    pub flow: f32,
    pub points: Vec<[f32; 2]>,
}

impl Default for Stroke {
    fn default() -> Self {
        Self { erase: false, size: 0.05, feather: 50.0, flow: 100.0, points: Vec::new() }
    }
}

/// Rasterises brush strokes into a `w × h` 0…1 plane by stamping soft
/// discs along each stroke.
pub fn rasterize(strokes: &[Stroke], w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0f32; w * h];
    let long = w.max(h) as f32;
    for st in strokes {
        let r = (st.size * long).max(0.5);
        let inner = r * (1.0 - st.feather.clamp(0.0, 100.0) / 100.0);
        let flow = st.flow.clamp(0.0, 100.0) / 100.0;
        let pts: Vec<[f32; 2]> = st.points.iter().map(|p| [p[0] * w as f32, p[1] * h as f32]).collect();
        if pts.is_empty() {
            continue;
        }
        let mut stamps = Vec::new();
        for (i, p) in pts.iter().enumerate() {
            match pts.get(i + 1) {
                Some(q) => {
                    let d = ((q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2)).sqrt();
                    let n = (d / (r * 0.25).max(0.5)).ceil().max(1.0) as usize;
                    stamps.extend((0..n).map(|k| {
                        let t = k as f32 / n as f32;
                        [p[0] + (q[0] - p[0]) * t, p[1] + (q[1] - p[1]) * t]
                    }));
                }
                None => stamps.push(*p),
            }
        }
        // This stroke's coverage (max of its stamps, over its bounding box),
        // merged once so a stroke doesn't build up where stamps overlap.
        let span = |v: f32, max: usize| (v.floor().max(0.0) as usize).min(max);
        let bx0 = span(stamps.iter().map(|p| p[0]).fold(f32::MAX, f32::min) - r, w);
        let bx1 = span(stamps.iter().map(|p| p[0]).fold(f32::MIN, f32::max) + r + 1.0, w);
        let by0 = span(stamps.iter().map(|p| p[1]).fold(f32::MAX, f32::min) - r, h);
        let by1 = span(stamps.iter().map(|p| p[1]).fold(f32::MIN, f32::max) + r + 1.0, h);
        if bx0 >= bx1 || by0 >= by1 {
            continue;
        }
        let bw = bx1 - bx0;
        let mut cover = vec![0f32; bw * (by1 - by0)];
        for [cx, cy] in stamps {
            let (x0, x1) = (span(cx - r, w).max(bx0), span(cx + r + 1.0, w).min(bx1));
            let (y0, y1) = (span(cy - r, h).max(by0), span(cy + r + 1.0, h).min(by1));
            for y in y0..y1 {
                for x in x0..x1 {
                    let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                    if d >= r {
                        continue;
                    }
                    let v = if d <= inner { 1.0 } else { 1.0 - smoothstep(inner, r, d) } * flow;
                    let c = &mut cover[(y - by0) * bw + (x - bx0)];
                    *c = c.max(v);
                }
            }
        }
        for y in by0..by1 {
            for x in bx0..bx1 {
                let c = cover[(y - by0) * bw + (x - bx0)];
                let o = &mut out[y * w + x];
                *o = if st.erase { *o * (1.0 - c) } else { *o + (1.0 - *o) * c };
            }
        }
    }
    out
}

impl Mask {
    pub fn from_gray(label: &str, img: &image::GrayImage) -> Mask {
        use base64::Engine;
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(img.as_raw(), img.width(), img.height(), image::ExtendedColorType::L8)
            .expect("in-memory PNG");
        Mask::Bitmap {
            label: label.to_string(),
            width: img.width(),
            height: img.height(),
            data: base64::engine::general_purpose::STANDARD.encode(png),
        }
    }

    /// The bitmap as a 0…1 plane, if this is a valid bitmap mask.
    pub fn bitmap_plane(&self) -> Option<Buf> {
        use base64::Engine;
        let Mask::Bitmap { data, .. } = self else { return None };
        let png = base64::engine::general_purpose::STANDARD.decode(data).ok()?;
        let img = image::load_from_memory(&png).ok()?.to_luma8();
        let (w, h) = (img.width() as usize, img.height() as usize);
        let px = img.as_raw().iter().map(|&v| [v as f32 / 255.0; 3]).collect();
        Some(Buf { w, h, px })
    }
}

/// Bilinear resample of a 0…1 plane (channel 0 of `b`) to `w × h`.
pub(crate) fn resample(b: &Buf, w: usize, h: usize) -> Vec<f32> {
    let (sx, sy) = (b.w as f32 / w as f32, b.h as f32 / h as f32);
    (0..w * h).map(|i| b.sample(((i % w) as f32 + 0.5) * sx - 0.5, ((i / w) as f32 + 0.5) * sy - 0.5)[0]).collect()
}

pub fn evaluate(mask: &Mask, img: &Buf) -> Vec<f32> {
    let (w, h) = (img.w, img.h);
    if let Mask::Bitmap { .. } = mask {
        return match mask.bitmap_plane() {
            Some(plane) => resample(&plane, w, h),
            None => vec![0.0; w * h],
        };
    }
    if let Mask::Brush { strokes } = mask {
        return rasterize(strokes, w, h);
    }
    match *mask {
        Mask::Bitmap { .. } | Mask::Brush { .. } => unreachable!(),
        Mask::Full => vec![1.0; w * h],
        Mask::Linear { x0, y0, x1, y1 } => {
            let (dx, dy) = ((x1 - x0) * w as f32, (y1 - y0) * h as f32);
            let len2 = (dx * dx + dy * dy).max(1e-6);
            (0..w * h)
                .map(|i| {
                    let px = (i % w) as f32 - x0 * w as f32;
                    let py = (i / w) as f32 - y0 * h as f32;
                    let t = (px * dx + py * dy) / len2;
                    1.0 - smoothstep(0.0, 1.0, t)
                })
                .collect()
        }
        Mask::Radial { cx, cy, rx, ry, feather } => {
            let f = (feather / 100.0).clamp(0.01, 1.0);
            let (rx, ry) = ((rx * w as f32).max(1.0), (ry * h as f32).max(1.0));
            (0..w * h)
                .map(|i| {
                    let nx = ((i % w) as f32 - cx * w as f32) / rx;
                    let ny = ((i / w) as f32 - cy * h as f32) / ry;
                    let d = (nx * nx + ny * ny).sqrt();
                    1.0 - smoothstep(1.0 - f, 1.0, d)
                })
                .collect()
        }
        Mask::Sky { sensitivity } => sky(img, sensitivity),
    }
}

/// Weight for optional luminance targeting.
pub fn luma_weight(range: [f32; 2], l: f32) -> f32 {
    let (lo, hi) = (range[0].min(range[1]), range[0].max(range[1]));
    let soft = 0.08;
    smoothstep(lo - soft, lo + soft, l) * (1.0 - smoothstep(hi - soft, hi + soft, l))
}

/// Heuristic sky mask (0…1) for an 8-bit display image.
pub fn sky_from_rgb(img: &image::RgbImage, sensitivity: f32) -> Vec<f32> {
    let px = img.pixels().map(|p| p.0.map(|v| v as f32 / 255.0)).collect();
    sky(&Buf { w: img.width() as usize, h: img.height() as usize, px }, sensitivity)
}

/// Sky heuristic on display-space pixels:
/// 1. per-pixel "sky-likeness" (blue hue band, or bright low-saturation
///    overcast), biased towards the top of the frame;
/// 2. column-wise flood from the top edge: sky must be connected to the
///    top, so blue shirts/water lower down are rejected;
/// 3. feathered with a blur.
fn sky(img: &Buf, sensitivity: f32) -> Vec<f32> {
    let (w, h) = (img.w, img.h);
    let tol = 0.35 + sensitivity.clamp(0.0, 100.0) / 100.0 * 0.4;
    let score: Vec<f32> = img
        .px
        .iter()
        .enumerate()
        .map(|(i, &p)| {
            let [hue, s, l] = rgb_to_hsl(p);
            let blue = if (180.0..=260.0).contains(&hue) { s.min(1.0) * smoothstep(0.25, 0.55, l) } else { 0.0 };
            let overcast = (1.0 - smoothstep(0.05, 0.25, s)) * smoothstep(0.65, 0.9, l);
            let vertical = 1.0 - (i / w) as f32 / h as f32;
            (blue.max(overcast) * (0.6 + 0.4 * vertical)).min(1.0)
        })
        .collect();

    let mut m = vec![0f32; w * h];
    let gap = (h / 60).max(2);
    for x in 0..w {
        let mut misses = 0;
        for y in 0..h {
            let s = score[y * w + x];
            if s >= 1.0 - tol {
                misses = 0;
                m[y * w + x] = 1.0;
            } else {
                misses += 1;
                if misses > gap {
                    break;
                }
            }
        }
    }
    let sigma = (w.max(h) as f32 / 150.0).max(1.0);
    let mut out = blur_plane(&m, w, h, sigma);
    // Re-sharpen the transition a little after blurring and gate by the raw
    // score so foreground edges stay clean.
    for (o, s) in out.iter_mut().zip(&score) {
        *o = smoothstep(0.2, 0.8, *o) * (0.5 + 0.5 * s.min(1.0));
    }
    out
}

/// Mean luminance under a mask (useful for auto-tuning a masked edit).
pub fn masked_mean_luma(img: &Buf, mask: &[f32]) -> f32 {
    let (sum, wsum) = img.px.iter().zip(mask).fold((0.0f32, 0.0f32), |(s, ws), (p, m)| (s + luma(*p) * m, ws + m));
    if wsum > 0.0 {
        sum / wsum
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn landscape(w: usize, h: usize) -> Buf {
        let mut b = Buf::new(w, h);
        for y in 0..h {
            for x in 0..w {
                b.px[y * w + x] = if y < h / 2 {
                    [0.35, 0.6, 0.95] // sky
                } else if x > w / 3 && x < w / 2 && y > 3 * h / 4 {
                    [0.2, 0.3, 0.9] // blue object on the ground
                } else {
                    [0.3, 0.45, 0.15] // grass
                };
            }
        }
        b
    }

    #[test]
    fn sky_mask_selects_top_not_blue_ground_object() {
        let img = landscape(120, 80);
        let m = evaluate(&Mask::Sky { sensitivity: 50.0 }, &img);
        let at = |x: usize, y: usize| m[y * 120 + x];
        assert!(at(60, 10) > 0.7, "sky {}", at(60, 10));
        assert!(at(60, 60) < 0.1, "grass {}", at(60, 60));
        assert!(at(50, 75) < 0.1, "blue object {}", at(50, 75));
    }

    #[test]
    fn radial_and_linear_shapes() {
        let img = Buf::new(100, 100);
        let r = evaluate(&Mask::Radial { cx: 0.5, cy: 0.5, rx: 0.3, ry: 0.3, feather: 20.0 }, &img);
        assert!(r[50 * 100 + 50] > 0.99 && r[5 * 100 + 5] < 0.01);
        let l = evaluate(&Mask::Linear { x0: 0.5, y0: 0.0, x1: 0.5, y1: 1.0 }, &img);
        assert!(l[0] > 0.99 && l[99 * 100] < 0.01 && l[50 * 100] > 0.3);
    }

    #[test]
    fn bitmap_mask_roundtrip() {
        let g = image::GrayImage::from_fn(40, 20, |x, _| image::Luma([if x < 20 { 255 } else { 0 }]));
        let m = Mask::from_gray("subject", &g);
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"kind\":\"bitmap\""));
        let back: Mask = serde_json::from_str(&json).unwrap();
        let v = evaluate(&back, &Buf::new(80, 40));
        assert!(v[10] > 0.99 && v[79] < 0.01);
    }

    #[test]
    fn luma_weight_band() {
        assert!(luma_weight([0.4, 0.6], 0.5) > 0.99);
        assert!(luma_weight([0.4, 0.6], 0.1) < 0.01);
    }
}
