//! Small image-processing primitives shared by the pipeline and masks.

use rayon::prelude::*;

/// RGB float image, row-major.
#[derive(Clone)]
pub struct Buf {
    pub w: usize,
    pub h: usize,
    pub px: Vec<[f32; 3]>,
}

impl Buf {
    pub fn new(w: usize, h: usize) -> Self {
        Self { w, h, px: vec![[0.0; 3]; w * h] }
    }

    /// Bilinear sample with edge clamping.
    #[inline]
    pub fn sample(&self, x: f32, y: f32) -> [f32; 3] {
        let x = x.clamp(0.0, (self.w - 1) as f32);
        let y = y.clamp(0.0, (self.h - 1) as f32);
        let x0 = x.floor() as usize;
        let y0 = y.floor() as usize;
        let x1 = (x0 + 1).min(self.w - 1);
        let y1 = (y0 + 1).min(self.h - 1);
        let fx = x - x0 as f32;
        let fy = y - y0 as f32;
        let p = |xx: usize, yy: usize| self.px[yy * self.w + xx];
        let (a, b, c, d) = (p(x0, y0), p(x1, y0), p(x0, y1), p(x1, y1));
        std::array::from_fn(|i| {
            let top = a[i] + (b[i] - a[i]) * fx;
            let bot = c[i] + (d[i] - c[i]) * fx;
            top + (bot - top) * fy
        })
    }
}

#[inline]
pub fn luma(p: [f32; 3]) -> f32 {
    0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]
}

#[inline]
pub fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Approximate Gaussian blur of a single-channel plane: three box passes
/// per axis. `sigma` in pixels.
pub fn blur_plane(src: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    if sigma < 0.3 || w == 0 || h == 0 {
        return src.to_vec();
    }
    // Box radius giving the requested sigma after 3 passes.
    let r = (((12.0 * sigma * sigma / 3.0) + 1.0).sqrt() / 2.0).floor().max(1.0) as usize;
    let mut a = src.to_vec();
    let mut b = vec![0f32; a.len()];
    for _ in 0..3 {
        box_rows(&a, &mut b, w, r);
        std::mem::swap(&mut a, &mut b);
    }
    let mut t = transpose(&a, w, h);
    let mut u = vec![0f32; t.len()];
    for _ in 0..3 {
        box_rows(&t, &mut u, h, r);
        std::mem::swap(&mut t, &mut u);
    }
    transpose(&t, h, w)
}

fn box_rows(src: &[f32], dst: &mut [f32], w: usize, r: usize) {
    dst.par_chunks_mut(w).zip(src.par_chunks(w)).for_each(|(out, row)| {
        let n = (2 * r + 1) as f32;
        let at = |i: isize| row[i.clamp(0, w as isize - 1) as usize];
        let mut acc: f32 = (-(r as isize)..=r as isize).map(at).sum();
        for (x, o) in out.iter_mut().enumerate() {
            *o = acc / n;
            let xi = x as isize;
            acc += at(xi + r as isize + 1) - at(xi - r as isize);
        }
    });
}

fn transpose(src: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0f32; w * h];
    out.par_chunks_mut(h).enumerate().for_each(|(x, col)| {
        for (y, v) in col.iter_mut().enumerate() {
            *v = src[y * w + x];
        }
    });
    out
}

// ------------------------------------------------------------ colour models

/// RGB (0…1) → HSL with hue in degrees.
#[inline]
pub fn rgb_to_hsl(p: [f32; 3]) -> [f32; 3] {
    let [r, g, b] = p;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d < 1e-6 {
        return [0.0, 0.0, l];
    }
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    [h * 60.0, s.clamp(0.0, 1.0), l]
}

#[inline]
pub fn hsl_to_rgb(hsl: [f32; 3]) -> [f32; 3] {
    let [h, s, l] = hsl;
    if s <= 1e-6 {
        return [l, l, l];
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let h = h.rem_euclid(360.0) / 360.0;
    let f = |t: f32| {
        let t = t.rem_euclid(1.0);
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    [f(h + 1.0 / 3.0), f(h), f(h - 1.0 / 3.0)]
}

/// Monotone cubic (Fritsch–Carlson) interpolation through `points`,
/// sampled into a LUT of `n` entries over x ∈ [0, 1].
pub fn curve_lut(points: &[[f32; 2]], n: usize) -> Vec<f32> {
    let mut pts: Vec<[f32; 2]> = points.iter().map(|[x, y]| [x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)]).collect();
    pts.sort_by(|a, b| a[0].total_cmp(&b[0]));
    pts.dedup_by(|a, b| (a[0] - b[0]).abs() < 1e-5);
    if pts.len() < 2 {
        return (0..n).map(|i| i as f32 / (n - 1) as f32).collect();
    }
    let k = pts.len();
    let d: Vec<f32> = (0..k - 1).map(|i| (pts[i + 1][1] - pts[i][1]) / (pts[i + 1][0] - pts[i][0])).collect();
    let mut m = vec![0f32; k];
    m[0] = d[0];
    m[k - 1] = d[k - 2];
    for i in 1..k - 1 {
        m[i] = if d[i - 1] * d[i] <= 0.0 { 0.0 } else { (d[i - 1] + d[i]) / 2.0 };
    }
    for i in 0..k - 1 {
        if d[i].abs() < 1e-9 {
            m[i] = 0.0;
            m[i + 1] = 0.0;
        } else {
            let a = m[i] / d[i];
            let b = m[i + 1] / d[i];
            let s = a * a + b * b;
            if s > 9.0 {
                let t = 3.0 / s.sqrt();
                m[i] = t * a * d[i];
                m[i + 1] = t * b * d[i];
            }
        }
    }
    (0..n)
        .map(|i| {
            let x = i as f32 / (n - 1) as f32;
            if x <= pts[0][0] {
                return pts[0][1];
            }
            if x >= pts[k - 1][0] {
                return pts[k - 1][1];
            }
            let j = pts.partition_point(|p| p[0] <= x) - 1;
            let (x0, y0, x1, y1) = (pts[j][0], pts[j][1], pts[j + 1][0], pts[j + 1][1]);
            let hh = x1 - x0;
            let t = (x - x0) / hh;
            let (t2, t3) = (t * t, t * t * t);
            let v = (2.0 * t3 - 3.0 * t2 + 1.0) * y0
                + (t3 - 2.0 * t2 + t) * hh * m[j]
                + (-2.0 * t3 + 3.0 * t2) * y1
                + (t3 - t2) * hh * m[j + 1];
            v.clamp(0.0, 1.0)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsl_roundtrip() {
        for p in [[1.0, 0.0, 0.0], [0.2, 0.5, 0.8], [0.9, 0.9, 0.1], [0.3, 0.3, 0.3]] {
            let q = hsl_to_rgb(rgb_to_hsl(p));
            for i in 0..3 {
                assert!((p[i] - q[i]).abs() < 1e-4, "{p:?} -> {q:?}");
            }
        }
    }

    #[test]
    fn blur_preserves_mean_and_smooths() {
        let (w, h) = (64, 32);
        let src: Vec<f32> = (0..w * h).map(|i| if (i % w) < w / 2 { 0.0 } else { 1.0 }).collect();
        let out = blur_plane(&src, w, h, 3.0);
        let mean_in: f32 = src.iter().sum::<f32>() / src.len() as f32;
        let mean_out: f32 = out.iter().sum::<f32>() / out.len() as f32;
        assert!((mean_in - mean_out).abs() < 0.02);
        let edge = out[10 * w + w / 2];
        assert!(edge > 0.2 && edge < 0.8);
    }

    #[test]
    fn identity_curve_and_monotone_s_curve() {
        let id = curve_lut(&[[0.0, 0.0], [1.0, 1.0]], 256);
        assert!(id.iter().enumerate().all(|(i, v)| (v - i as f32 / 255.0).abs() < 1e-4));
        let s = curve_lut(&[[0.0, 0.0], [0.25, 0.15], [0.75, 0.85], [1.0, 1.0]], 256);
        assert!(s.windows(2).all(|w| w[1] >= w[0] - 1e-6));
        assert!(s[64] < 64.0 / 255.0 && s[192] > 192.0 / 255.0);
    }
}
