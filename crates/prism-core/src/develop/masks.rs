//! Mask providers for local adjustments. A mask is a per-pixel weight in
//! 0…1 computed on the (geometry-transformed) image. Providers only decide
//! *where*; the adjustment engine is shared. New providers (e.g. an ML
//! subject/sky segmenter) plug in here without touching the pipeline.

use super::buffer::{blur_plane, luma, rgb_to_hsl, smoothstep, Buf};
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
}

pub fn evaluate(mask: &Mask, img: &Buf) -> Vec<f32> {
    let (w, h) = (img.w, img.h);
    match *mask {
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
    fn luma_weight_band() {
        assert!(luma_weight([0.4, 0.6], 0.5) > 0.99);
        assert!(luma_weight([0.4, 0.6], 0.1) < 0.01);
    }
}
