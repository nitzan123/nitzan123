//! Full-resolution demosaicing of a single-channel CFA (colour filter array)
//! plane into RGB.
//!
//! * Bayer (2×2 pattern): Malvar–He–Cutler gradient-corrected linear
//!   interpolation (ICASSP 2004). Sharp, artefact-light and fast.
//! * Any other pattern (e.g. Fuji X-Trans 6×6): distance-weighted average of
//!   same-colour samples in a 5×5 window.
//!
//! Input values are already black/white normalised and white balanced;
//! `color(y, x)` returns 0 = R, 1 = G, 2 = B for absolute sensor coordinates.

use rayon::prelude::*;

/// A CFA plane cut out of the sensor. `(oy, ox)` is the plane's origin in
/// absolute sensor coordinates so the pattern lookup stays aligned.
pub struct Plane<'a> {
    pub data: &'a [f32],
    pub w: usize,
    pub h: usize,
    pub oy: usize,
    pub ox: usize,
}

impl Plane<'_> {
    /// Mirror-reflects out-of-range indices. Reflecting about the edge pixel
    /// preserves the parity of the index, so the CFA colour at the reflected
    /// position matches for 2×2 patterns.
    #[inline]
    fn at(&self, y: isize, x: isize) -> f32 {
        let r = |i: isize, n: usize| -> usize {
            let n = n as isize;
            let mut i = i;
            if i < 0 {
                i = -i;
            }
            if i >= n {
                i = 2 * n - 2 - i;
            }
            i.clamp(0, n - 1) as usize
        };
        self.data[r(y, self.h) * self.w + r(x, self.w)]
    }
}

pub fn demosaic(p: &Plane<'_>, bayer: bool, color: &(dyn Fn(usize, usize) -> usize + Sync)) -> Vec<f32> {
    if bayer {
        malvar(p, color)
    } else {
        generic(p, color)
    }
}

// Malvar–He–Cutler kernels (×8), indexed [dy+2][dx+2].
const G_AT_RB: [[f32; 5]; 5] =
    [[0., 0., -1., 0., 0.], [0., 0., 2., 0., 0.], [-1., 2., 4., 2., -1.], [0., 0., 2., 0., 0.], [0., 0., -1., 0., 0.]];
/// R (or B) at a G pixel whose horizontal neighbours are R (or B).
const RB_AT_G_ROW: [[f32; 5]; 5] = [
    [0., 0., 0.5, 0., 0.],
    [0., -1., 0., -1., 0.],
    [-1., 4., 5., 4., -1.],
    [0., -1., 0., -1., 0.],
    [0., 0., 0.5, 0., 0.],
];
/// R (or B) at a G pixel whose vertical neighbours are R (or B).
const RB_AT_G_COL: [[f32; 5]; 5] = [
    [0., 0., -1., 0., 0.],
    [0., -1., 4., -1., 0.],
    [0.5, 0., 5., 0., 0.5],
    [0., -1., 4., -1., 0.],
    [0., 0., -1., 0., 0.],
];
/// R at B, or B at R.
const RB_AT_BR: [[f32; 5]; 5] = [
    [0., 0., -1.5, 0., 0.],
    [0., 2., 0., 2., 0.],
    [-1.5, 0., 6., 0., -1.5],
    [0., 2., 0., 2., 0.],
    [0., 0., -1.5, 0., 0.],
];

fn malvar(p: &Plane<'_>, color: &(dyn Fn(usize, usize) -> usize + Sync)) -> Vec<f32> {
    let mut out = vec![0f32; p.w * p.h * 3];
    out.par_chunks_mut(p.w * 3).enumerate().for_each(|(y, row)| {
        let yi = y as isize;
        for x in 0..p.w {
            let xi = x as isize;
            let c = color(p.oy + y, p.ox + x);
            let conv = |k: &[[f32; 5]; 5]| -> f32 {
                let mut s = 0.0;
                for (dy, kr) in k.iter().enumerate() {
                    for (dx, &kv) in kr.iter().enumerate() {
                        if kv != 0.0 {
                            s += kv * p.at(yi + dy as isize - 2, xi + dx as isize - 2);
                        }
                    }
                }
                (s / 8.0).max(0.0)
            };
            let v = p.at(yi, xi);
            let mut rgb = [0f32; 3];
            rgb[c] = v;
            if c == 1 {
                // Which of R/B sits left/right of this green?
                let horiz = color(p.oy + y, p.ox + x + 1);
                let (h_c, v_c) = if horiz == 0 { (0, 2) } else { (2, 0) };
                rgb[h_c] = conv(&RB_AT_G_ROW);
                rgb[v_c] = conv(&RB_AT_G_COL);
            } else {
                rgb[1] = conv(&G_AT_RB);
                rgb[2 - c] = conv(&RB_AT_BR);
            }
            row[x * 3..x * 3 + 3].copy_from_slice(&rgb);
        }
    });
    out
}

fn generic(p: &Plane<'_>, color: &(dyn Fn(usize, usize) -> usize + Sync)) -> Vec<f32> {
    let mut out = vec![0f32; p.w * p.h * 3];
    out.par_chunks_mut(p.w * 3).enumerate().for_each(|(y, row)| {
        for x in 0..p.w {
            let mut acc = [0f32; 3];
            let mut wsum = [0f32; 3];
            for dy in -2isize..=2 {
                for dx in -2isize..=2 {
                    let (yy, xx) = (y as isize + dy, x as isize + dx);
                    if yy < 0 || xx < 0 || yy >= p.h as isize || xx >= p.w as isize {
                        continue;
                    }
                    let c = color(p.oy + yy as usize, p.ox + xx as usize);
                    let wgt = 1.0 / (1.0 + (dy * dy + dx * dx) as f32);
                    acc[c] += wgt * p.data[yy as usize * p.w + xx as usize];
                    wsum[c] += wgt;
                }
            }
            let own = color(p.oy + y, p.ox + x);
            for c in 0..3 {
                row[x * 3 + c] = if c == own {
                    p.data[y * p.w + x]
                } else if wsum[c] > 0.0 {
                    acc[c] / wsum[c]
                } else {
                    acc[1] / wsum[1].max(1e-6)
                };
            }
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smooth colour field with a few hard edges, in 0..1.
    fn truth(w: usize, h: usize) -> Vec<f32> {
        let mut v = vec![0f32; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let fx = x as f32 / w as f32;
                let fy = y as f32 / h as f32;
                let edge = if (x / 17 + y / 23) % 2 == 0 { 0.15 } else { 0.0 };
                v[(y * w + x) * 3] = 0.2 + 0.6 * fx + edge;
                v[(y * w + x) * 3 + 1] = 0.3 + 0.4 * (fx * 6.0 + fy * 3.0).sin().abs() * 0.5 + edge;
                v[(y * w + x) * 3 + 2] = 0.7 - 0.5 * fy + edge;
            }
        }
        v
    }

    fn mosaic(rgb: &[f32], w: usize, h: usize, color: &dyn Fn(usize, usize) -> usize) -> Vec<f32> {
        (0..w * h).map(|i| rgb[i * 3 + color(i / w, i % w)]).collect()
    }

    fn psnr(a: &[f32], b: &[f32], w: usize, h: usize) -> f64 {
        // Ignore a 3 px border.
        let mut se = 0f64;
        let mut n = 0f64;
        for y in 3..h - 3 {
            for x in 3..w - 3 {
                for c in 0..3 {
                    let d = (a[(y * w + x) * 3 + c] - b[(y * w + x) * 3 + c]) as f64;
                    se += d * d;
                    n += 1.0;
                }
            }
        }
        10.0 * (1.0 / (se / n)).log10()
    }

    #[test]
    fn malvar_reconstructs_bayer_accurately() {
        let (w, h) = (96, 64);
        let rggb = |y: usize, x: usize| match (y % 2, x % 2) {
            (0, 0) => 0,
            (1, 1) => 2,
            _ => 1,
        };
        let t = truth(w, h);
        let m = mosaic(&t, w, h, &rggb);
        let out = demosaic(&Plane { data: &m, w, h, oy: 0, ox: 0 }, true, &rggb);
        let q = psnr(&out, &t, w, h);
        assert!(q > 30.0, "PSNR {q:.1} dB");
        // Measured pixels pass through untouched.
        assert_eq!(out[0], t[0]);
    }

    #[test]
    fn origin_offset_keeps_pattern_alignment() {
        // Plane starting at an odd sensor column must still use the right colours.
        let (w, h) = (64, 64);
        let rggb = |y: usize, x: usize| match (y % 2, x % 2) {
            (0, 0) => 0,
            (1, 1) => 2,
            _ => 1,
        };
        let shifted = |y: usize, x: usize| rggb(y, x + 1);
        let t = truth(w, h);
        let m = mosaic(&t, w, h, &shifted);
        let out = demosaic(&Plane { data: &m, w, h, oy: 0, ox: 1 }, true, &rggb);
        assert!(psnr(&out, &t, w, h) > 30.0);
    }

    #[test]
    fn generic_handles_xtrans() {
        // Fuji X-Trans 6×6 layout (0=R,1=G,2=B).
        const XT: [[usize; 6]; 6] = [
            [1, 1, 0, 1, 1, 2],
            [1, 1, 2, 1, 1, 0],
            [2, 0, 1, 0, 2, 1],
            [1, 1, 2, 1, 1, 0],
            [1, 1, 0, 1, 1, 2],
            [0, 2, 1, 2, 0, 1],
        ];
        let xt = |y: usize, x: usize| XT[y % 6][x % 6];
        let (w, h) = (96, 72);
        let t = truth(w, h);
        let m = mosaic(&t, w, h, &xt);
        let out = demosaic(&Plane { data: &m, w, h, oy: 0, ox: 0 }, false, &xt);
        assert!(psnr(&out, &t, w, h) > 22.0, "PSNR {:.1}", psnr(&out, &t, w, h));
        assert!(out.iter().all(|v| v.is_finite()));
    }
}
