//! Spot retouching: clone (copy pixels from a source spot) and heal (copy
//! the source's texture but blend its colour and brightness into the
//! target's surroundings, so dust and blemishes disappear without patches).
//!
//! Spots are stored in *source image* coordinates and applied before lens
//! correction and geometry, so they stay put when the photo is cropped,
//! rotated or straightened afterwards.

use super::buffer::{smoothstep, Buf};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SpotMode {
    #[default]
    Heal,
    Clone,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Spot {
    pub enabled: bool,
    pub mode: SpotMode,
    /// Target centre, normalised to the source image (0…1).
    pub x: f32,
    pub y: f32,
    /// Where pixels are taken from, normalised.
    pub sx: f32,
    pub sy: f32,
    /// Radius as a fraction of the image's long edge.
    pub radius: f32,
    /// Soft edge 0…100.
    pub feather: f32,
    /// Strength 0…100.
    pub opacity: f32,
}

impl Default for Spot {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: SpotMode::Heal,
            x: 0.5,
            y: 0.5,
            sx: 0.4,
            sy: 0.5,
            radius: 0.02,
            feather: 40.0,
            opacity: 100.0,
        }
    }
}

/// Correction field over a spot's bounding box (see [`membrane`]).
struct Membrane {
    x0: usize,
    y0: usize,
    w: usize,
    v: Vec<[f32; 3]>,
}

impl Membrane {
    fn at(&self, x: usize, y: usize) -> [f32; 3] {
        self.v[(y - self.y0 + 1) * self.w + (x - self.x0 + 1)]
    }
}

/// Solves Laplace's equation inside the disc `(tx, ty, r)` with the
/// target − source difference as boundary values, on a grid one pixel
/// larger than the disc's bounding box.
fn membrane(b: &Buf, tx: f32, ty: f32, r: f32, ox: f32, oy: f32) -> Membrane {
    let x0 = (tx - r).floor().max(0.0) as usize;
    let y0 = (ty - r).floor().max(0.0) as usize;
    let x1 = ((tx + r).ceil() as usize + 1).min(b.w);
    let y1 = ((ty + r).ceil() as usize + 1).min(b.h);
    let (gw, gh) = (x1 - x0 + 2, y1 - y0 + 2);
    let inside = |gx: usize, gy: usize| {
        let (fx, fy) = ((gx + x0) as f32 - 0.5, (gy + y0) as f32 - 0.5);
        gx > 0 && gy > 0 && gx < gw - 1 && gy < gh - 1 && ((fx - tx).powi(2) + (fy - ty).powi(2)).sqrt() < r
    };
    let mut v = vec![[0f32; 3]; gw * gh];
    let mut interior = Vec::new();
    let mut boundary_sum = [0f32; 3];
    let mut boundary_n = 0f32;
    for gy in 0..gh {
        for gx in 0..gw {
            let i = gy * gw + gx;
            if inside(gx, gy) {
                interior.push(i);
                continue;
            }
            // Only cells next to the interior matter as boundary values.
            let near = [(0i32, 1i32), (0, -1), (1, 0), (-1, 0)].iter().any(|(dx, dy)| {
                let (nx, ny) = (gx as i32 + dx, gy as i32 + dy);
                nx >= 0 && ny >= 0 && (nx as usize) < gw && (ny as usize) < gh && inside(nx as usize, ny as usize)
            });
            if near {
                let (px, py) = ((gx + x0) as f32 - 1.0, (gy + y0) as f32 - 1.0);
                let t = sample_avg(b, px, py);
                let s = sample_avg(b, px + ox, py + oy);
                v[i] = std::array::from_fn(|c| t[c] - s[c]);
                for c in 0..3 {
                    boundary_sum[c] += v[i][c];
                }
                boundary_n += 1.0;
            }
        }
    }
    // Start from the mean boundary difference, then relax (SOR).
    let mean = boundary_sum.map(|s| s / boundary_n.max(1.0));
    for &i in &interior {
        v[i] = mean;
    }
    let iterations = ((r * 2.0) as usize).clamp(40, 400);
    let omega = 1.9;
    for _ in 0..iterations {
        for &i in &interior {
            let n = [v[i - 1], v[i + 1], v[i - gw], v[i + gw]];
            for c in 0..3 {
                let avg = (n[0][c] + n[1][c] + n[2][c] + n[3][c]) * 0.25;
                v[i][c] += omega * (avg - v[i][c]);
            }
        }
    }
    Membrane { x0, y0, w: gw, v }
}

/// Average of a 3×3 neighbourhood (reduces noise in boundary samples).
fn sample_avg(b: &Buf, x: f32, y: f32) -> [f32; 3] {
    let mut acc = [0f32; 3];
    for dy in [-1.0, 0.0, 1.0] {
        for dx in [-1.0, 0.0, 1.0] {
            let p = b.sample(x + dx, y + dy);
            for c in 0..3 {
                acc[c] += p[c] / 9.0;
            }
        }
    }
    acc
}

/// Applies the enabled spots in order. Each spot sees the result of the
/// previous ones, so overlapping fixes compose naturally.
pub fn apply(b: &mut Buf, spots: &[Spot]) {
    let long = b.w.max(b.h) as f32;
    for s in spots.iter().filter(|s| s.enabled && s.radius > 0.0) {
        let r = (s.radius * long).max(1.0);
        let (tx, ty) = (s.x * b.w as f32, s.y * b.h as f32);
        let (ox, oy) = (s.sx * b.w as f32 - tx, s.sy * b.h as f32 - ty);
        if ox.abs() < 0.5 && oy.abs() < 0.5 {
            continue;
        }
        let inner = r * (1.0 - s.feather.clamp(0.0, 100.0) / 100.0 * 0.6);
        let opacity = s.opacity.clamp(0.0, 100.0) / 100.0;

        let x0 = (tx - r).floor().max(0.0) as usize;
        let x1 = ((tx + r).ceil() as usize + 1).min(b.w);
        let y0 = (ty - r).floor().max(0.0) as usize;
        let y1 = ((ty + r).ceil() as usize + 1).min(b.h);
        if x0 >= x1 || y0 >= y1 {
            continue;
        }
        // Heal: the colour difference between target and source, known
        // just outside the spot, is interpolated inwards as a membrane
        // (Laplace equation): the patch keeps the source's texture but
        // takes on the target's light and colour (Poisson-style healing).
        let correction = (s.mode == SpotMode::Heal).then(|| membrane(b, tx, ty, r, ox, oy));
        // Read everything first: the source may overlap the target.
        let mut patch = Vec::with_capacity((x1 - x0) * (y1 - y0));
        for y in y0..y1 {
            for x in x0..x1 {
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                let d = ((fx - tx).powi(2) + (fy - ty).powi(2)).sqrt();
                if d >= r {
                    continue;
                }
                let w = if d <= inner { 1.0 } else { 1.0 - smoothstep(inner, r, d) } * opacity;
                let mut src = b.sample(fx + ox - 0.5, fy + oy - 0.5);
                if let Some(m) = &correction {
                    let c = m.at(x, y);
                    for k in 0..3 {
                        src[k] = (src[k] + c[k]).max(0.0);
                    }
                }
                patch.push((y * b.w + x, src, w));
            }
        }
        for (i, src, w) in patch {
            let p = &mut b.px[i];
            for c in 0..3 {
                p[c] += (src[c] - p[c]) * w;
            }
        }
    }
}

/// [`auto_source`] on a linear image with `spots` already applied.
pub fn auto_source_in(img: &crate::raw::LinearImage, spots: &[Spot], x: f32, y: f32, radius: f32) -> [f32; 2] {
    let mut b = Buf { w: img.width() as usize, h: img.height() as usize, px: img.as_raw().as_chunks::<3>().0.to_vec() };
    apply(&mut b, spots);
    auto_source(&b, x, y, radius)
}

/// Picks where to copy from for a spot at `(x, y)` (normalised, radius as
/// a fraction of the long edge): the nearby position whose surroundings
/// best match the target's, so the patch blends in. Returns normalised
/// source coordinates inside the image.
pub fn auto_source(b: &Buf, x: f32, y: f32, radius: f32) -> [f32; 2] {
    let long = b.w.max(b.h) as f32;
    let r = (radius * long).max(1.0);
    let (tx, ty) = (x * b.w as f32, y * b.h as f32);
    let ring: Vec<[f32; 2]> = (0..32)
        .flat_map(|i| {
            let a = i as f32 / 32.0 * std::f32::consts::TAU;
            [1.1f32, 1.5].map(|k| [a.cos() * r * k, a.sin() * r * k])
        })
        .collect();
    let target: Vec<[f32; 3]> = ring.iter().map(|[dx, dy]| b.sample(tx + dx, ty + dy)).collect();
    let fits = |cx: f32, cy: f32| {
        cx - r * 1.5 >= 0.0 && cy - r * 1.5 >= 0.0 && cx + r * 1.5 < b.w as f32 && cy + r * 1.5 < b.h as f32
    };
    let mut best: Option<(f32, [f32; 2])> = None;
    for dist in [2.2f32, 3.0, 4.0] {
        for i in 0..24 {
            let a = i as f32 / 24.0 * std::f32::consts::TAU;
            let (cx, cy) = (tx + a.cos() * r * dist, ty + a.sin() * r * dist);
            if !fits(cx, cy) {
                continue;
            }
            let mut cost: f32 = ring
                .iter()
                .zip(&target)
                .map(|([dx, dy], t)| {
                    let p = b.sample(cx + dx, cy + dy);
                    (0..3).map(|c| (p[c] - t[c]).powi(2)).sum::<f32>()
                })
                .sum();
            cost *= 1.0 + 0.05 * dist; // prefer closer sources on ties
            if best.is_none_or(|(c, _)| cost < c) {
                best = Some((cost, [cx, cy]));
            }
        }
    }
    let [cx, cy] = best.map(|b| b.1).unwrap_or_else(|| {
        // Spot too big for any in-frame source: mirror across the centre.
        [(b.w as f32 - tx).clamp(0.0, b.w as f32), ty]
    });
    [cx / b.w as f32, cy / b.h as f32]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Horizontal brightness gradient with a dark "dust spot" at (60, 50).
    fn dusty() -> Buf {
        let (w, h) = (120, 100);
        let mut b = Buf::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let base = 0.2 + 0.6 * x as f32 / w as f32;
                let d = ((x as f32 - 60.0).powi(2) + (y as f32 - 50.0).powi(2)).sqrt();
                let v = if d < 5.0 { 0.02 } else { base };
                b.px[y * w + x] = [v, v * 0.9, v * 0.8];
            }
        }
        b
    }

    fn spot(mode: SpotMode) -> Spot {
        // Source 30 px to the left: darker part of the gradient.
        Spot {
            mode,
            x: 60.0 / 120.0,
            y: 0.5,
            sx: 30.0 / 120.0,
            sy: 0.5,
            radius: 9.0 / 120.0,
            feather: 30.0,
            ..Default::default()
        }
    }

    #[test]
    fn auto_source_prefers_matching_surroundings() {
        // Left half dark, right half bright: a spot in the bright half must
        // take its source from the bright half, in frame.
        let (w, h) = (200, 100);
        let mut b = Buf::new(w, h);
        for y in 0..h {
            for x in 0..w {
                b.px[y * w + x] = [if x < 100 { 0.1 } else { 0.8 }; 3];
            }
        }
        let [sx, sy] = auto_source(&b, 0.6, 0.5, 0.04);
        assert!(sx > 0.55 && (0.0..=1.0).contains(&sy), "{sx} {sy}");
        assert!(sx * 200.0 - 1.5 * 8.0 >= 100.0, "whole source neighbourhood is bright: {sx}");
    }

    #[test]
    fn heal_matches_clean_image_on_a_vertical_gradient() {
        // Sky-like vertical gradient, dust at the centre, source below.
        let (w, h) = (200, 200);
        let clean = |x: usize, y: usize| {
            let _ = x;
            let v = 0.15 + 0.5 * (y as f32 / h as f32).powf(1.8);
            [v * 0.7, v * 0.85, v]
        };
        let mut b = Buf::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let d = ((x as f32 - 100.0).powi(2) + (y as f32 - 80.0).powi(2)).sqrt();
                b.px[y * w + x] = if d < 6.0 { [0.01; 3] } else { clean(x, y) };
            }
        }
        let spot = Spot { x: 0.5, y: 0.4, sx: 0.5, sy: 0.64, radius: 0.1, feather: 40.0, ..Default::default() };
        apply(&mut b, &[spot]);
        let mut worst = 0f32;
        for y in 60..100 {
            for x in 80..120 {
                let (p, q) = (b.px[y * w + x], clean(x, y));
                for c in 0..3 {
                    worst = worst.max((p[c] - q[c]).abs() / q[c]);
                }
            }
        }
        assert!(worst < 0.03, "worst relative error {worst}");
    }

    #[test]
    fn heal_removes_the_spot_and_matches_surroundings() {
        let mut b = dusty();
        apply(&mut b, &[spot(SpotMode::Heal)]);
        let expected = 0.2 + 0.6 * 60.0 / 120.0;
        let got = b.px[50 * 120 + 60][0];
        assert!((got - expected).abs() < 0.03, "healed centre {got} vs {expected}");
    }

    #[test]
    fn clone_copies_source_brightness_and_disabled_spots_do_nothing() {
        let mut b = dusty();
        apply(&mut b, &[spot(SpotMode::Clone)]);
        let src = 0.2 + 0.6 * 30.0 / 120.0;
        assert!((b.px[50 * 120 + 60][0] - src).abs() < 0.02, "clone copies the source as is");

        let mut b2 = dusty();
        apply(&mut b2, &[Spot { enabled: false, ..spot(SpotMode::Heal) }]);
        assert_eq!(b2.px, dusty().px);
    }
}
