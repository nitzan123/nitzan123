//! One-click "Auto" develop. Deterministic image statistics, not ML.
//!
//! * [`auto_settings`] — exposure and tonal range. White balance is left
//!   alone: the camera's as-shot balance is applied at decode time and is
//!   usually better than any statistic (gray-world fails on scenes dominated
//!   by one colour).
//! * [`auto_white_balance`] — opt-in, conservative gray-world correction.

use super::buffer::luma;
use super::pipeline::{wb_gains, WB_TEMP_STOPS, WB_TINT_STOPS};
use super::DevelopSettings;
use crate::raw::LinearImage;

fn sample(src: &LinearImage) -> Vec<[f32; 3]> {
    src.as_raw()
        .as_chunks::<3>()
        .0
        .iter()
        .step_by(((src.width() * src.height()) as usize / 200_000).max(1))
        .copied()
        .collect()
}

/// Gray-world over well-exposed midtones, applied at half strength and
/// clamped, so a strongly coloured scene is nudged rather than neutralised.
/// Returns `(temperature, tint)`.
pub fn auto_white_balance(src: &LinearImage) -> (f32, f32) {
    let (mut sr, mut sg, mut sb, mut n) = (0f64, 0f64, 0f64, 0f64);
    for p in sample(src) {
        let y = luma(p);
        if (0.02..0.7).contains(&y) && p.iter().all(|v| *v < 0.95) {
            sr += p[0] as f64;
            sg += p[1] as f64;
            sb += p[2] as f64;
            n += 1.0;
        }
    }
    if n < 100.0 || sr <= 0.0 || sg <= 0.0 || sb <= 0.0 {
        return (0.0, 0.0);
    }
    let (r, g, b) = (sr / n, sg / n, sb / n);
    // Exact inverse of `wb_gains`: r·2^t = b·2^-t  and  g·2^k = sqrt(r'·b').
    let temp = ((b / r).log2() / (2.0 * WB_TEMP_STOPS as f64) * 100.0) as f32;
    let gains = wb_gains(temp, 0.0);
    let (r2, g2, b2) = (r * gains[0] as f64, g * gains[1] as f64, b * gains[2] as f64);
    let tint = -(((r2 * b2).sqrt() / g2).log2() / WB_TINT_STOPS as f64 * 100.0) as f32;
    ((temp * 0.5).clamp(-35.0, 35.0), (tint * 0.5).clamp(-25.0, 25.0))
}

pub fn auto_settings(src: &LinearImage) -> DevelopSettings {
    let mut s = DevelopSettings::default();
    let px = sample(src);
    if px.is_empty() {
        return s;
    }

    // --- exposure: put the log-average luminance near middle grey
    let mut lum: Vec<f32> = px.iter().map(|p| luma(*p).max(1e-5)).collect();
    let log_avg = (lum.iter().map(|y| y.ln()).sum::<f32>() / lum.len() as f32).exp();
    s.light.exposure = ((0.18 / log_avg).log2() * 0.6).clamp(-1.5, 1.5);

    // --- range: recover clipped highlights, open deep shadows
    lum.sort_by(f32::total_cmp);
    let pct = |q: f32| lum[((lum.len() - 1) as f32 * q) as usize] * 2f32.powf(s.light.exposure);
    let (p02, p98) = (pct(0.02), pct(0.98));
    if p98 > 0.85 {
        s.light.highlights = (-(p98 - 0.85) * 300.0).clamp(-70.0, 0.0);
    } else if p98 < 0.5 {
        s.light.whites = ((0.5 - p98) * 120.0).clamp(0.0, 40.0);
    }
    if p02 < 0.004 {
        s.light.shadows = 25.0;
    } else if p02 > 0.03 {
        s.light.blacks = -20.0;
    }
    let range = (p98.max(1e-4) / p02.max(1e-4)).log2();
    if range < 5.0 {
        s.light.contrast = 15.0;
    }
    s.color.vibrance = 15.0;
    s.light.clarity = 10.0;
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_image_gets_brighter_and_wb_is_untouched() {
        let img = LinearImage::from_fn(64, 64, |x, _| {
            let v = 0.01 + x as f32 / 64.0 * 0.04;
            image::Rgb([v * 0.6, v, v * 1.6])
        });
        let s = auto_settings(&img);
        assert!(s.light.exposure > 0.5, "exposure {}", s.light.exposure);
        assert_eq!((s.white_balance.temperature, s.white_balance.tint), (0.0, 0.0));
    }

    #[test]
    fn neutral_mid_image_stays_near_neutral() {
        let img = LinearImage::from_fn(64, 64, |x, y| {
            let v = 0.05 + ((x + y) as f32 / 128.0) * 0.3;
            image::Rgb([v, v, v])
        });
        assert!(auto_settings(&img).light.exposure.abs() < 1.0);
        let (t, n) = auto_white_balance(&img);
        assert!(t.abs() < 1.0 && n.abs() < 1.0);
    }

    #[test]
    fn auto_wb_corrects_in_the_right_direction() {
        let cast = |r: f32, g: f32, b: f32| {
            LinearImage::from_fn(64, 64, move |x, _| {
                let v = 0.05 + x as f32 / 64.0 * 0.3;
                image::Rgb([v * r, v * g, v * b])
            })
        };
        let (t, _) = auto_white_balance(&cast(0.7, 1.0, 1.4));
        assert!(t > 5.0, "blue cast should be warmed, got {t}");
        let (t, _) = auto_white_balance(&cast(1.4, 1.0, 0.7));
        assert!(t < -5.0, "orange cast should be cooled, got {t}");
        let (_, n) = auto_white_balance(&cast(1.0, 1.4, 1.0));
        assert!(n > 5.0, "green cast should get +tint (magenta), got {n}");
        let (_, n) = auto_white_balance(&cast(1.3, 0.8, 1.3));
        assert!(n < -5.0, "magenta cast should get -tint (green), got {n}");
    }
}
