//! The render pipeline. Order (each stage is skipped when neutral):
//!
//! | # | Stage | Space |
//! |---|-------|-------|
//! | 0 | lens: vignetting, distortion, chromatic aberration | linear |
//! | 1 | geometry: quarter turns, flip, straighten, crop | linear |
//! | 2 | white balance, exposure | linear |
//! | 3 | highlights / shadows / whites / blacks / contrast + highlight roll-off | luminance, perceptual |
//! | 4 | clarity (local contrast) | luminance, perceptual |
//! | 5 | → display sRGB | |
//! | 6 | tone curve | display |
//! | 7 | 8-band colour mixer, vibrance, saturation | HSL |
//! | 8 | split toning | display |
//! | 9 | local adjustments (masked) | display |
//! | 10 | vignette | display |
//! | 11 | noise reduction, sharpening | luminance |
//!
//! Spatial radii (clarity, sky feather) scale with image size so a preview
//! and a full-size export look the same; sharpening/noise radii are in
//! output pixels, as photographers expect.

use super::buffer::{blur_plane, curve_lut, hsl_to_rgb, luma, rgb_to_hsl, smoothstep, Buf};
use super::masks::{evaluate, luma_weight};
use super::{DevelopSettings, Geometry, BAND_HUES};
use crate::imaging::{linear_to_srgb, srgb_to_linear};
use crate::raw::LinearImage;
use image::{Rgb32FImage, RgbImage};
use rayon::prelude::*;

const GAMMA: f32 = 2.2;

/// Renders `src` (linear light) to a display-space (sRGB, 0…1) image.
pub fn render(src: &LinearImage, s: &DevelopSettings) -> Rgb32FImage {
    let buf = Buf { w: src.width() as usize, h: src.height() as usize, px: src.as_raw().as_chunks::<3>().0.to_vec() };
    let mut buf = buf;
    super::retouch::apply(&mut buf, &s.spots);
    let bitmaps = align_bitmap_masks(&buf, s);
    let buf = crate::lens::apply(buf, &s.lens);
    let mut buf = geometry(buf, &s.geometry);
    linear_stage(&mut buf, s);
    clarity(&mut buf, s.light.clarity);
    to_display(&mut buf);
    curve(&mut buf, s);
    color(&mut buf, s);
    split_tone(&mut buf, s);
    local(&mut buf, s, &bitmaps);
    vignette(&mut buf, s);
    detail(&mut buf, s);
    let flat: Vec<f32> = buf.px.iter().flat_map(|p| p.iter().map(|v| v.clamp(0.0, 1.0))).collect();
    Rgb32FImage::from_raw(buf.w as u32, buf.h as u32, flat).expect("buffer size")
}

pub fn render_to_rgb8(src: &LinearImage, s: &DevelopSettings) -> RgbImage {
    let f = render(src, s);
    let (w, h) = f.dimensions();
    let bytes = f.as_raw().iter().map(|v| (v * 255.0 + 0.5) as u8).collect();
    let mut out = RgbImage::from_raw(w, h, bytes).expect("buffer size");
    if let Some(t) = &s.text {
        t.draw(&mut out);
    }
    out
}

/// Linear RGB gains for the temperature/tint sliders. Shared with auto
/// white balance so the two stay exact inverses.
pub fn wb_gains(temperature: f32, tint: f32) -> [f32; 3] {
    let t = temperature / 100.0 * WB_TEMP_STOPS;
    let g = -tint / 100.0 * WB_TINT_STOPS;
    let r = 2f32.powf(t);
    let b = 2f32.powf(-t);
    let gg = 2f32.powf(g);
    // Keep overall brightness roughly constant.
    let norm = 0.2126 * r + 0.7152 * gg + 0.0722 * b;
    [r / norm, gg / norm, b / norm]
}
pub const WB_TEMP_STOPS: f32 = 0.6;
pub const WB_TINT_STOPS: f32 = 0.4;

// --------------------------------------------------------------- 1 geometry

fn geometry(mut b: Buf, g: &Geometry) -> Buf {
    for _ in 0..(g.rotate90 % 4) {
        b = rotate_cw(&b);
    }
    if g.flip_horizontal {
        for row in b.px.chunks_mut(b.w) {
            row.reverse();
        }
    }
    if g.straighten.abs() > 0.01 {
        b = straighten(&b, g.straighten.clamp(-45.0, 45.0));
    }
    if let Some(c) = g.crop.map(|c| c.clamped()) {
        let x0 = ((c.x * b.w as f32).round() as usize).min(b.w - 1);
        let y0 = ((c.y * b.h as f32).round() as usize).min(b.h - 1);
        let cw = ((c.w * b.w as f32).round() as usize).clamp(1, b.w - x0);
        let ch = ((c.h * b.h as f32).round() as usize).clamp(1, b.h - y0);
        let mut out = Buf::new(cw, ch);
        for y in 0..ch {
            out.px[y * cw..(y + 1) * cw].copy_from_slice(&b.px[(y0 + y) * b.w + x0..(y0 + y) * b.w + x0 + cw]);
        }
        b = out;
    }
    b
}

fn rotate_cw(b: &Buf) -> Buf {
    let mut out = Buf::new(b.h, b.w);
    for y in 0..b.h {
        for x in 0..b.w {
            out.px[x * b.h + (b.h - 1 - y)] = b.px[y * b.w + x];
        }
    }
    out
}

/// Rotates by `deg` and scales up just enough to hide the empty corners.
fn straighten(b: &Buf, deg: f32) -> Buf {
    let (w, h) = (b.w as f32, b.h as f32);
    let th = deg.to_radians();
    let (sin, cos) = (th.sin().abs(), th.cos());
    let scale = (cos + (h / w) * sin).max(cos + (w / h) * sin);
    let (cx, cy) = (w / 2.0, h / 2.0);
    let (s, c) = (th.sin(), th.cos());
    let mut out = Buf::new(b.w, b.h);
    out.px.par_chunks_mut(b.w).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            let u = (x as f32 + 0.5 - cx) / scale;
            let v = (y as f32 + 0.5 - cy) / scale;
            // inverse rotation
            let sx = c * u + s * v + cx - 0.5;
            let sy = -s * u + c * v + cy - 0.5;
            *o = b.sample(sx, sy);
        }
    });
    out
}

// ------------------------------------------------------------- 2–3 linear

fn linear_stage(b: &mut Buf, s: &DevelopSettings) {
    let wb = wb_gains(s.white_balance.temperature, s.white_balance.tint);
    let gain = 2f32.powf(s.light.exposure.clamp(-5.0, 5.0));
    let l = &s.light;
    let (hl, sh, wh, bl, ct) =
        (l.highlights / 100.0, l.shadows / 100.0, l.whites / 100.0, l.blacks / 100.0, l.contrast / 100.0);
    let tone_active = [hl, sh, wh, bl, ct].iter().any(|v| v.abs() > 1e-4);
    // Roll-off only when the user pushed light around; an unadjusted image
    // (e.g. crop-only edit) must keep its exact tones, incl. pure white.
    let rolloff = tone_active
        || s.light.exposure.abs() > 1e-4
        || s.white_balance.temperature.abs() > 1e-3
        || s.white_balance.tint.abs() > 1e-3;

    b.px.par_iter_mut().for_each(|p| {
        let mut q = [p[0] * wb[0] * gain, p[1] * wb[1] * gain, p[2] * wb[2] * gain];
        let y = luma(q).max(0.0);
        if y > 1e-6 {
            let mut v = y.powf(1.0 / GAMMA);
            if tone_active {
                v += sh * 0.30 * (1.0 - smoothstep(0.0, 0.55, v)) * smoothstep(0.0, 0.08, v);
                v += hl * 0.30 * smoothstep(0.45, 1.1, v);
                v += wh * 0.18 * smoothstep(0.55, 1.0, v);
                v += bl * 0.12 * (1.0 - smoothstep(0.0, 0.3, v));
                if ct.abs() > 1e-4 {
                    let sc = smoothstep(0.0, 1.0, v.clamp(0.0, 1.0));
                    v += ct * (sc - v.clamp(0.0, 1.0)) * 1.5;
                }
            }
            // Soft roll-off so blown channels compress instead of clipping hard.
            if rolloff && v > 0.9 {
                v = 0.9 + 0.1 * (1.0 - (-(v - 0.9) / 0.1).exp());
            }
            let ny = v.max(0.0).powf(GAMMA);
            let k = ny / y;
            q = [q[0] * k, q[1] * k, q[2] * k];
        }
        *p = q;
    });
}

// ---------------------------------------------------------------- 4 clarity

fn clarity(b: &mut Buf, amount: f32) {
    if amount.abs() < 0.5 {
        return;
    }
    let k = amount / 100.0;
    let p: Vec<f32> = b.px.iter().map(|q| luma(*q).max(0.0).powf(1.0 / GAMMA)).collect();
    let blurred = blur_plane(&p, b.w, b.h, b.w.max(b.h) as f32 / 80.0);
    b.px.par_iter_mut().zip(p.par_iter().zip(blurred.par_iter())).for_each(|(q, (&v, &bl))| {
        if v < 1e-5 {
            return;
        }
        let mid = (4.0 * v * (1.0 - v)).clamp(0.0, 1.0);
        let nv = (v + k * 0.8 * (v - bl) * mid).max(0.0);
        let g = (nv / v).powf(GAMMA);
        *q = [q[0] * g, q[1] * g, q[2] * g];
    });
}

fn to_display(b: &mut Buf) {
    b.px.par_iter_mut().for_each(|p| {
        for v in p.iter_mut() {
            *v = linear_to_srgb(v.clamp(0.0, 1.0));
        }
    });
}

// ------------------------------------------------------------------ 6 curve

fn curve(b: &mut Buf, s: &DevelopSettings) {
    if s.curve.is_identity() {
        return;
    }
    let lut = curve_lut(&s.curve.points, 1024);
    b.px.par_iter_mut().for_each(|p| {
        for v in p.iter_mut() {
            *v = lut[(v.clamp(0.0, 1.0) * 1023.0).round() as usize];
        }
    });
}

// ------------------------------------------------------------------ 7 colour

/// Smooth weight of `hue` for each mixer band (weights sum to 1).
pub fn band_weights(hue: f32) -> [f32; 8] {
    let mut w = [0f32; 8];
    for i in 0..8 {
        let c = BAND_HUES[i];
        let prev = BAND_HUES[(i + 7) % 8];
        let next = BAND_HUES[(i + 1) % 8];
        let d = ((hue - c + 540.0) % 360.0) - 180.0; // signed distance
        let span = if d < 0.0 { (c - prev + 360.0) % 360.0 } else { (next - c + 360.0) % 360.0 };
        let t = (d.abs() / span).min(1.0);
        w[i] = 0.5 + 0.5 * (std::f32::consts::PI * t).cos();
        if t >= 1.0 {
            w[i] = 0.0;
        }
    }
    let sum: f32 = w.iter().sum();
    if sum > 0.0 {
        w.iter_mut().for_each(|v| *v /= sum);
    }
    w
}

fn color(b: &mut Buf, s: &DevelopSettings) {
    let m = &s.mixer;
    let mixer_active = m.hue.iter().chain(&m.saturation).chain(&m.luminance).any(|v| v.abs() > 1e-3);
    let vib = s.color.vibrance / 100.0;
    let sat = s.color.saturation / 100.0;
    if !mixer_active && vib.abs() < 1e-4 && sat.abs() < 1e-4 {
        return;
    }
    b.px.par_iter_mut().for_each(|p| {
        let [mut h, mut sv, mut l] = rgb_to_hsl(*p);
        if mixer_active && sv > 1e-4 {
            let w = band_weights(h);
            let dot = |a: &[f32; 8]| a.iter().zip(&w).map(|(x, y)| x * y).sum::<f32>() / 100.0;
            h += dot(&m.hue) * 30.0;
            sv *= 1.0 + dot(&m.saturation);
            l += dot(&m.luminance) * 0.25 * sv.min(1.0);
        }
        if vib.abs() > 1e-4 {
            sv = if vib > 0.0 { sv + vib * (1.0 - sv) * sv * 1.2 } else { sv * (1.0 + vib) };
        }
        if sat.abs() > 1e-4 {
            sv *= 1.0 + sat;
        }
        *p = hsl_to_rgb([h, sv.clamp(0.0, 1.0), l.clamp(0.0, 1.0)]);
    });
}

// ------------------------------------------------------------- 8 split tone

fn split_tone(b: &mut Buf, s: &DevelopSettings) {
    let st = &s.split_tone;
    if st.highlight_saturation < 0.5 && st.shadow_saturation < 0.5 {
        return;
    }
    let hi = hsl_to_rgb([st.highlight_hue, 1.0, 0.5]);
    let sh = hsl_to_rgb([st.shadow_hue, 1.0, 0.5]);
    let (hs, ss) = (st.highlight_saturation / 100.0, st.shadow_saturation / 100.0);
    let pivot = 0.5 - st.balance / 100.0 * 0.35;
    b.px.par_iter_mut().for_each(|p| {
        let l = luma(*p);
        let wh = smoothstep(pivot - 0.25, pivot + 0.45, l) * hs;
        let ws = (1.0 - smoothstep(pivot - 0.45, pivot + 0.25, l)) * ss;
        for c in 0..3 {
            p[c] += (hi[c] - 0.5) * 0.3 * wh + (sh[c] - 0.5) * 0.3 * ws;
        }
    });
}

// ------------------------------------------------------------------ 9 local

/// Bitmap masks cover the uncropped photo; run each through the same lens
/// geometry and crop/rotate as the image (at reduced size) so it lines up
/// with the rendered pixels.
/// Maps normalised points between the source photo and the developed
/// frame (after lens correction and geometry). `to_source` maps from the
/// developed frame back to the source; otherwise source → developed, with
/// `None` for points cropped out. `aspect` is the source width / height.
pub fn map_points(aspect: f32, s: &DevelopSettings, points: &[[f32; 2]], to_source: bool) -> Vec<Option<[f32; 2]>> {
    // A coordinate grid pushed through the same transforms as the pixels.
    let long = 768.0;
    let (gw, gh) = if aspect >= 1.0 {
        (long as usize, ((long / aspect) as usize).max(2))
    } else {
        (((long * aspect) as usize).max(2), long as usize)
    };
    let mut grid = Buf::new(gw, gh);
    for y in 0..gh {
        for x in 0..gw {
            grid.px[y * gw + x] = [(x as f32 + 0.5) / gw as f32, (y as f32 + 0.5) / gh as f32, 1.0];
        }
    }
    let mut lens = s.lens.clone();
    (lens.vignetting, lens.manual_vignetting, lens.ca, lens.manual_ca_red, lens.manual_ca_blue) =
        (0.0, 0.0, 0.0, 0.0, 0.0);
    let d = geometry(crate::lens::apply(grid, &lens), &s.geometry);
    points
        .iter()
        .map(|&[px, py]| {
            if to_source {
                let v = d.sample(px * d.w as f32 - 0.5, py * d.h as f32 - 0.5);
                return Some([v[0], v[1]]);
            }
            let (mut best, mut at) = (f32::MAX, 0);
            for (i, v) in d.px.iter().enumerate() {
                let e = (v[0] - px).powi(2) + (v[1] - py).powi(2);
                if e < best {
                    (best, at) = (e, i);
                }
            }
            // Further than ~2 grid cells from any visible pixel: cropped out.
            let cell = 2.0 / gw.min(gh) as f32;
            (best.sqrt() < cell)
                .then(|| [((at % d.w) as f32 + 0.5) / d.w as f32, ((at / d.w) as f32 + 0.5) / d.h as f32])
        })
        .collect()
}

fn align_bitmap_masks(src: &Buf, s: &DevelopSettings) -> Vec<Option<Buf>> {
    let mut lens_geometry = s.lens.clone();
    lens_geometry.vignetting = 0.0;
    lens_geometry.manual_vignetting = 0.0;
    s.local
        .iter()
        .map(|adj| {
            if !adj.enabled {
                return None;
            }
            let plane = adj.mask.bitmap_plane()?;
            // Same aspect as the source, long edge ≤ 1024.
            let k = (1024.0 / src.w.max(src.h) as f32).min(1.0);
            let (w, h) = (((src.w as f32 * k) as usize).max(1), ((src.h as f32 * k) as usize).max(1));
            let px = super::masks::resample(&plane, w, h).into_iter().map(|v| [v; 3]).collect();
            let m = crate::lens::apply(Buf { w, h, px }, &lens_geometry);
            Some(geometry(m, &s.geometry))
        })
        .collect()
}

fn local(b: &mut Buf, s: &DevelopSettings, bitmaps: &[Option<Buf>]) {
    for (adj, aligned) in s.local.iter().zip(bitmaps).filter(|(a, _)| a.enabled) {
        let mut mask = match aligned {
            Some(m) => super::masks::resample(m, b.w, b.h),
            None => evaluate(&adj.mask, b),
        };
        if adj.invert {
            mask.iter_mut().for_each(|m| *m = 1.0 - *m);
        }
        if let Some(r) = adj.luma_range {
            for (m, p) in mask.iter_mut().zip(&b.px) {
                *m *= luma_weight(r, luma(*p));
            }
        }
        let blurred = (adj.blur > 0.5).then(|| {
            let sigma = adj.blur / 100.0 * b.w.max(b.h) as f32 / 60.0;
            let planes: Vec<Vec<f32>> =
                (0..3).map(|c| blur_plane(&b.px.iter().map(|p| p[c]).collect::<Vec<_>>(), b.w, b.h, sigma)).collect();
            planes
        });
        let ev = adj.exposure;
        let (ct, sat) = (adj.contrast / 100.0, adj.saturation / 100.0);
        let (temp, tint) = (adj.temperature, adj.tint);
        b.px.par_iter_mut().zip(mask.par_iter()).enumerate().for_each(|(i, (p, &m))| {
            if m < 1e-3 {
                return;
            }
            if let Some(bl) = &blurred {
                for c in 0..3 {
                    p[c] += (bl[c][i] - p[c]) * m;
                }
            }
            if ev.abs() > 1e-4 || temp.abs() > 1e-3 || tint.abs() > 1e-3 {
                let g = wb_gains(temp * m, tint * m);
                let e = 2f32.powf(ev * m);
                for c in 0..3 {
                    let lin = srgb_to_linear(p[c].clamp(0.0, 1.0)) * g[c] * e;
                    let lin = if lin > 0.9 { 0.9 + 0.1 * (1.0 - (-(lin - 0.9) / 0.1).exp()) } else { lin };
                    p[c] = linear_to_srgb(lin);
                }
            }
            if ct.abs() > 1e-4 {
                for v in p.iter_mut() {
                    *v = 0.5 + (*v - 0.5) * (1.0 + ct * m);
                }
            }
            if sat.abs() > 1e-4 {
                let [h, sv, l] = rgb_to_hsl([p[0].clamp(0.0, 1.0), p[1].clamp(0.0, 1.0), p[2].clamp(0.0, 1.0)]);
                *p = hsl_to_rgb([h, (sv * (1.0 + sat * m)).clamp(0.0, 1.0), l]);
            }
        });
    }
}

// --------------------------------------------------------------- 10 vignette

fn vignette(b: &mut Buf, s: &DevelopSettings) {
    let v = &s.vignette;
    if v.amount.abs() < 0.5 {
        return;
    }
    let amt = v.amount / 100.0;
    let start = v.midpoint.clamp(0.0, 100.0) / 100.0 * 0.9;
    let width = 0.05 + v.feather.clamp(0.0, 100.0) / 100.0 * 0.9;
    let (w, h) = (b.w as f32, b.h as f32);
    b.px.par_chunks_mut(b.w).enumerate().for_each(|(y, row)| {
        let dy = (y as f32 + 0.5) / h * 2.0 - 1.0;
        for (x, p) in row.iter_mut().enumerate() {
            let dx = (x as f32 + 0.5) / w * 2.0 - 1.0;
            let d = ((dx * dx + dy * dy) / 2.0).sqrt();
            let f = smoothstep(start, start + width, d);
            for c in p.iter_mut() {
                *c = if amt < 0.0 { *c * (1.0 + amt * f) } else { *c + (1.0 - *c) * amt * f };
            }
        }
    });
}

// ----------------------------------------------------------------- 11 detail

fn detail(b: &mut Buf, s: &DevelopSettings) {
    let d = &s.detail;
    let nr = d.noise_reduction.clamp(0.0, 100.0) / 100.0;
    let sharpen = d.sharpen_amount.clamp(0.0, 150.0) / 100.0;
    if nr < 1e-3 && sharpen < 1e-3 {
        return;
    }
    let y: Vec<f32> = b.px.iter().map(|p| luma(*p)).collect();
    let mut ny = y.clone();
    if nr > 1e-3 {
        let bl = blur_plane(&ny, b.w, b.h, 0.8 + nr * 1.7);
        for (v, &m) in ny.iter_mut().zip(&bl) {
            let edge = (-(*v - m).abs() * 25.0).exp();
            *v += (m - *v) * nr * edge;
        }
    }
    if sharpen > 1e-3 {
        let bl = blur_plane(&ny, b.w, b.h, d.sharpen_radius.clamp(0.5, 3.0));
        let thr = d.sharpen_threshold.clamp(0.0, 100.0) / 255.0 * 0.1;
        for (v, &m) in ny.iter_mut().zip(&bl) {
            let diff = *v - m;
            if diff.abs() > thr {
                *v += diff * sharpen * 1.5;
            }
        }
    }
    b.px.par_iter_mut().zip(y.par_iter().zip(ny.par_iter())).for_each(|(p, (&o, &n))| {
        let delta = n - o;
        for c in p.iter_mut() {
            *c += delta;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::develop::{masks::Mask, CropRect, LocalAdjustment};
    use crate::imaging::srgb_to_linear;

    fn grey(w: u32, h: u32, v: f32) -> LinearImage {
        LinearImage::from_pixel(w, h, image::Rgb([v, v, v]))
    }

    fn mean(img: &Rgb32FImage) -> [f32; 3] {
        let n = (img.width() * img.height()) as f32;
        let mut m = [0f32; 3];
        for p in img.pixels() {
            for (acc, v) in m.iter_mut().zip(p.0) {
                *acc += v / n;
            }
        }
        m
    }

    #[test]
    fn identity_settings_roundtrip_to_srgb() {
        let lin = srgb_to_linear(0.5);
        let out = render(&grey(16, 16, lin), &DevelopSettings::default());
        assert!((out.get_pixel(3, 3).0[0] - 0.5).abs() < 0.01);
    }

    #[test]
    fn exposure_brightens_and_rolloff_never_exceeds_one() {
        let mut s = DevelopSettings::default();
        s.light.exposure = 1.0;
        let base = mean(&render(&grey(8, 8, 0.1), &DevelopSettings::default()))[0];
        let up = mean(&render(&grey(8, 8, 0.1), &s))[0];
        assert!(up > base + 0.1, "{base} -> {up}");
        s.light.exposure = 5.0;
        let hot = render(&grey(8, 8, 0.8), &s);
        assert!(hot.pixels().all(|p| p.0[0] <= 1.0));
    }

    #[test]
    fn temperature_warms() {
        let mut s = DevelopSettings::default();
        s.white_balance.temperature = 60.0;
        let m = mean(&render(&grey(8, 8, 0.2), &s));
        assert!(m[0] > m[2] + 0.05, "{m:?}");
    }

    #[test]
    fn shadows_lift_darks_more_than_brights() {
        let mut s = DevelopSettings::default();
        s.light.shadows = 100.0;
        let id = DevelopSettings::default();
        let dark_gain = mean(&render(&grey(4, 4, 0.02), &s))[0] - mean(&render(&grey(4, 4, 0.02), &id))[0];
        let bright_gain = mean(&render(&grey(4, 4, 0.7), &s))[0] - mean(&render(&grey(4, 4, 0.7), &id))[0];
        assert!(dark_gain > 0.05 && bright_gain.abs() < 0.02, "{dark_gain} {bright_gain}");
    }

    #[test]
    fn mixer_targets_only_its_band() {
        let mut img = LinearImage::new(2, 1);
        img.put_pixel(0, 0, image::Rgb([srgb_to_linear(0.2), srgb_to_linear(0.7), srgb_to_linear(0.2)])); // green
        img.put_pixel(1, 0, image::Rgb([srgb_to_linear(0.8), srgb_to_linear(0.1), srgb_to_linear(0.1)])); // red
        let mut s = DevelopSettings::default();
        s.mixer.saturation[3] = -100.0; // desaturate greens
        let out = render(&img, &s);
        let g = out.get_pixel(0, 0).0;
        let r = out.get_pixel(1, 0).0;
        assert!((g[0] - g[1]).abs() < 0.05, "green desaturated {g:?}");
        assert!(r[0] - r[1] > 0.5, "red untouched {r:?}");
    }

    #[test]
    fn band_weights_are_normalised() {
        for h in (0..360).step_by(7) {
            let w = band_weights(h as f32);
            assert!((w.iter().sum::<f32>() - 1.0).abs() < 1e-4);
        }
        assert!(band_weights(120.0)[3] > 0.99);
    }

    #[test]
    fn points_map_both_ways_through_rotation_and_crop() {
        let mut s = DevelopSettings::default();
        let same = map_points(1.5, &s, &[[0.25, 0.75]], true)[0].unwrap();
        assert!((same[0] - 0.25).abs() < 0.01 && (same[1] - 0.75).abs() < 0.01);

        s.geometry.rotate90 = 1; // 90° clockwise
        s.geometry.crop = Some(CropRect { x: 0.0, y: 0.0, w: 0.5, h: 1.0 });
        // Developed top-left came from the source's bottom-left.
        let src = map_points(1.5, &s, &[[0.0, 0.0]], true)[0].unwrap();
        assert!(src[0] < 0.01 && src[1] > 0.99, "{src:?}");
        // Round trip for a point that is still visible…
        let shown = map_points(1.5, &s, &[[0.2, 0.9]], false)[0].unwrap();
        let back = map_points(1.5, &s, &[shown], true)[0].unwrap();
        assert!((back[0] - 0.2).abs() < 0.01 && (back[1] - 0.9).abs() < 0.01, "{back:?}");
        // …and a cropped-out one has no developed position.
        assert!(map_points(1.5, &s, &[[0.5, 0.1]], false)[0].is_none());
    }

    #[test]
    fn brush_mask_and_spots_render() {
        use crate::develop::masks::Stroke;
        use crate::develop::retouch::Spot;
        let mut s = DevelopSettings::default();
        s.local.push(LocalAdjustment {
            mask: Mask::Brush {
                strokes: vec![
                    Stroke { points: vec![[0.1, 0.5], [0.9, 0.5]], size: 0.1, feather: 0.0, ..Default::default() },
                    Stroke { erase: true, points: vec![[0.5, 0.5]], size: 0.1, feather: 0.0, ..Default::default() },
                ],
            },
            exposure: 1.0,
            ..Default::default()
        });
        let out = render(&grey(100, 50, 0.1), &s);
        let v = |x, y| out.get_pixel(x, y).0[0];
        assert!(v(25, 25) > v(25, 5) + 0.1, "painted band is brighter");
        assert!((v(50, 25) - v(25, 5)).abs() < 0.01, "erased middle is untouched");
        assert!(!s.is_identity());
        s.local.clear();
        s.spots.push(Spot { enabled: false, ..Default::default() });
        assert!(s.is_identity(), "disabled spots change nothing");
    }

    #[test]
    fn geometry_rotate_and_crop_dimensions() {
        let mut s = DevelopSettings::default();
        s.geometry.rotate90 = 1;
        s.geometry.crop = Some(CropRect { x: 0.0, y: 0.0, w: 0.5, h: 0.5 });
        let out = render(&grey(40, 20, 0.2), &s);
        assert_eq!(out.dimensions(), (10, 20));
        s.geometry.straighten = 10.0;
        let out = render(&grey(40, 20, 0.2), &s);
        assert_eq!(out.dimensions(), (10, 20));
        // Straighten must not introduce empty (black) corners.
        assert!(out.pixels().all(|p| p.0[0] > 0.3));
    }

    #[test]
    fn local_radial_exposure_is_masked() {
        let mut s = DevelopSettings::default();
        s.local.push(LocalAdjustment {
            mask: Mask::Radial { cx: 0.5, cy: 0.5, rx: 0.2, ry: 0.2, feather: 10.0 },
            exposure: 1.5,
            ..Default::default()
        });
        let out = render(&grey(50, 50, 0.1), &s);
        assert!(out.get_pixel(25, 25).0[0] > out.get_pixel(2, 2).0[0] + 0.15);
    }

    #[test]
    fn crop_only_edit_keeps_pure_white_and_edge_crops_do_not_panic() {
        let mut s = DevelopSettings::default();
        s.geometry.crop = Some(CropRect { x: 0.1, y: 0.1, w: 0.5, h: 0.5 });
        let out = render_to_rgb8(&grey(40, 40, 1.0), &s);
        assert!(out.pixels().all(|p| p.0 == [255, 255, 255]));
        for (w, h) in [(40, 40), (3, 2), (1, 1)] {
            s.geometry.crop = Some(CropRect { x: 0.999, y: 0.995, w: 0.5, h: 0.5 });
            let out = render(&grey(w, h, 0.2), &s);
            assert!(out.width() >= 1 && out.height() >= 1);
        }
    }

    #[test]
    fn bitmap_mask_follows_rotation() {
        // Mask covers the left half of the source; after a 90° CW turn the
        // masked half is at the top.
        let g = image::GrayImage::from_fn(40, 20, |x, _| image::Luma([if x < 20 { 255 } else { 0 }]));
        let mut s = DevelopSettings::default();
        s.geometry.rotate90 = 1;
        s.local.push(LocalAdjustment { mask: Mask::from_gray("subject", &g), exposure: 2.0, ..Default::default() });
        let out = render(&grey(40, 20, 0.05), &s);
        assert_eq!(out.dimensions(), (20, 40));
        assert!(out.get_pixel(10, 5).0[0] > out.get_pixel(10, 35).0[0] + 0.2);
    }

    #[test]
    fn vignette_darkens_corners_only() {
        let mut s = DevelopSettings::default();
        s.vignette.amount = -80.0;
        let out = render(&grey(60, 40, 0.3), &s);
        assert!(out.get_pixel(0, 0).0[0] < out.get_pixel(30, 20).0[0] - 0.1);
    }

    #[test]
    fn sharpening_increases_edge_contrast() {
        let mut img = LinearImage::new(40, 10);
        for (x, _, p) in img.enumerate_pixels_mut() {
            let v = if x < 20 { 0.1 } else { 0.4 };
            *p = image::Rgb([v, v, v]);
        }
        let mut s = DevelopSettings::default();
        s.detail.sharpen_amount = 100.0;
        let base = render(&img, &DevelopSettings::default());
        let sharp = render(&img, &s);
        let span = |o: &Rgb32FImage| o.get_pixel(21, 5).0[0] - o.get_pixel(18, 5).0[0];
        assert!(span(&sharp) > span(&base));
    }
}
