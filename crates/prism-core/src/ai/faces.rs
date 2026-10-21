//! Faces: YuNet detection (box + 5 landmarks) and SFace 128-d identity
//! embeddings on ArcFace-aligned 112×112 crops. Post-processing follows
//! OpenCV's `FaceDetectorYN` / `FaceRecognizerSF`.

use super::{load_plan, normalize, to_tensor, tract_err, Plan};
use crate::error::Result;
use image::RgbImage;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tract_onnx::prelude::*;

const DET: usize = 640;
const STRIDES: [usize; 3] = [8, 16, 32];
const SCORE_THRESHOLD: f32 = 0.7;
const NMS_IOU: f32 = 0.3;
/// SFace cosine similarity above which two faces are the same person
/// (OpenCV recommends 0.363; slightly stricter to avoid merging people).
pub const SAME_PERSON: f32 = 0.40;

/// ArcFace reference landmarks in a 112×112 crop: eyes, nose, mouth corners.
const TEMPLATE: [[f32; 2]; 5] =
    [[38.2946, 51.6963], [73.5318, 51.5014], [56.0252, 71.7366], [41.5493, 92.3655], [70.7299, 92.2041]];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Face {
    /// Box in image pixels.
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub score: f32,
    pub landmarks: [[f32; 2]; 5],
    #[serde(skip)]
    pub embedding: Vec<f32>,
}

pub struct FaceModels {
    detector: Plan,
    recognizer: Plan,
}

impl FaceModels {
    pub fn load(dir: &Path) -> Result<Self> {
        Ok(Self {
            detector: load_plan(&dir.join("yunet.onnx"), &[(f32::datum_type(), &[1, 3, DET, DET])])?,
            recognizer: load_plan(&dir.join("sface.onnx"), &[(f32::datum_type(), &[1, 3, 112, 112])])?,
        })
    }

    /// Detects faces and computes their embeddings.
    pub fn analyze(&self, img: &RgbImage) -> Result<Vec<Face>> {
        let mut faces = self.detect(img)?;
        for f in &mut faces {
            let crop = align(img, &f.landmarks);
            // SFace: RGB, 0..255, no normalisation.
            let out = self.recognizer.run(tvec![to_tensor(&crop, [0, 1, 2], |_, v| v).into()]).map_err(tract_err)?;
            let mut e: Vec<f32> = out[0].to_plain_array_view::<f32>().map_err(tract_err)?.iter().copied().collect();
            normalize(&mut e);
            f.embedding = e;
        }
        Ok(faces)
    }

    pub fn detect(&self, img: &RgbImage) -> Result<Vec<Face>> {
        let (w, h) = img.dimensions();
        let scale = DET as f32 / w.max(h).max(1) as f32;
        let (nw, nh) = (((w as f32 * scale) as u32).max(1), ((h as f32 * scale) as u32).max(1));
        let resized = image::imageops::resize(img, nw, nh, image::imageops::FilterType::Triangle);
        let mut canvas = RgbImage::new(DET as u32, DET as u32);
        image::imageops::replace(&mut canvas, &resized, 0, 0);
        // YuNet: BGR, 0..255.
        let out = self.detector.run(tvec![to_tensor(&canvas, [2, 1, 0], |_, v| v).into()]).map_err(tract_err)?;
        let view = |i: usize| -> Result<Vec<f32>> {
            Ok(out[i].to_plain_array_view::<f32>().map_err(tract_err)?.iter().copied().collect())
        };
        let mut cands = Vec::new();
        for (si, &stride) in STRIDES.iter().enumerate() {
            let (cls, obj, bbox, kps) = (view(si)?, view(3 + si)?, view(6 + si)?, view(9 + si)?);
            let cols = DET / stride;
            for idx in 0..cls.len() {
                let score = (cls[idx].clamp(0.0, 1.0) * obj[idx].clamp(0.0, 1.0)).sqrt();
                if score < SCORE_THRESHOLD {
                    continue;
                }
                let (r, c) = ((idx / cols) as f32, (idx % cols) as f32);
                let s = stride as f32;
                let cx = (c + bbox[idx * 4]) * s;
                let cy = (r + bbox[idx * 4 + 1]) * s;
                let bw = bbox[idx * 4 + 2].exp() * s;
                let bh = bbox[idx * 4 + 3].exp() * s;
                let mut lm = [[0f32; 2]; 5];
                for (k, p) in lm.iter_mut().enumerate() {
                    *p = [(kps[idx * 10 + 2 * k] + c) * s / scale, (kps[idx * 10 + 2 * k + 1] + r) * s / scale];
                }
                cands.push(Face {
                    x: (cx - bw / 2.0) / scale,
                    y: (cy - bh / 2.0) / scale,
                    w: bw / scale,
                    h: bh / scale,
                    score,
                    landmarks: lm,
                    embedding: Vec::new(),
                });
            }
        }
        Ok(nms(cands))
    }
}

fn iou(a: &Face, b: &Face) -> f32 {
    let x1 = a.x.max(b.x);
    let y1 = a.y.max(b.y);
    let x2 = (a.x + a.w).min(b.x + b.w);
    let y2 = (a.y + a.h).min(b.y + b.h);
    let inter = (x2 - x1).max(0.0) * (y2 - y1).max(0.0);
    inter / (a.w * a.h + b.w * b.h - inter).max(1e-6)
}

fn nms(mut c: Vec<Face>) -> Vec<Face> {
    c.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut keep: Vec<Face> = Vec::new();
    for f in c {
        if keep.iter().all(|k| iou(k, &f) < NMS_IOU) {
            keep.push(f);
        }
    }
    keep
}

/// Least-squares similarity transform (Umeyama, no reflection) mapping
/// `src` points onto `dst`. Returns `[a, b, tx, ty]` for
/// `x' = a·x − b·y + tx`, `y' = b·x + a·y + ty`.
pub fn similarity(src: &[[f32; 2]; 5], dst: &[[f32; 2]; 5]) -> [f32; 4] {
    let n = 5.0;
    let mean = |p: &[[f32; 2]; 5]| [p.iter().map(|q| q[0]).sum::<f32>() / n, p.iter().map(|q| q[1]).sum::<f32>() / n];
    let (ms, md) = (mean(src), mean(dst));
    let (mut sxx, mut sxy, mut var) = (0f32, 0f32, 0f32);
    for (s, d) in src.iter().zip(dst) {
        let (sx, sy) = (s[0] - ms[0], s[1] - ms[1]);
        let (dx, dy) = (d[0] - md[0], d[1] - md[1]);
        sxx += sx * dx + sy * dy;
        sxy += sx * dy - sy * dx;
        var += sx * sx + sy * sy;
    }
    let a = sxx / var.max(1e-9);
    let b = sxy / var.max(1e-9);
    [a, b, md[0] - (a * ms[0] - b * ms[1]), md[1] - (b * ms[0] + a * ms[1])]
}

/// Warps the face to the 112×112 ArcFace template.
pub fn align(img: &RgbImage, landmarks: &[[f32; 2]; 5]) -> RgbImage {
    let [a, b, tx, ty] = similarity(landmarks, &TEMPLATE);
    // Inverse of the similarity: scale 1/(a²+b²), rotation by −θ.
    let d = (a * a + b * b).max(1e-9);
    let (w, h) = (img.width() as f32, img.height() as f32);
    RgbImage::from_fn(112, 112, |x, y| {
        let (u, v) = (x as f32 - tx, y as f32 - ty);
        let sx = (a * u + b * v) / d;
        let sy = (-b * u + a * v) / d;
        if sx < 0.0 || sy < 0.0 || sx > w - 1.0 || sy > h - 1.0 {
            return image::Rgb([0, 0, 0]);
        }
        let (x0, y0) = (sx.floor() as u32, sy.floor() as u32);
        let (x1, y1) = ((x0 + 1).min(img.width() - 1), (y0 + 1).min(img.height() - 1));
        let (fx, fy) = (sx - x0 as f32, sy - y0 as f32);
        let p = |xx, yy| img.get_pixel(xx, yy).0;
        let (p00, p10, p01, p11) = (p(x0, y0), p(x1, y0), p(x0, y1), p(x1, y1));
        image::Rgb(std::array::from_fn(|c| {
            let top = p00[c] as f32 + (p10[c] as f32 - p00[c] as f32) * fx;
            let bot = p01[c] as f32 + (p11[c] as f32 - p01[c] as f32) * fx;
            (top + (bot - top) * fy).round() as u8
        }))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn similarity_recovers_known_transform() {
        // Rotate 30°, scale 2, translate (5, -3).
        let (s, t) = (2.0f32, 30f32.to_radians());
        let (a, b) = (s * t.cos(), s * t.sin());
        let dst: [[f32; 2]; 5] = std::array::from_fn(|i| {
            [a * TEMPLATE[i][0] - b * TEMPLATE[i][1] + 5.0, b * TEMPLATE[i][0] + a * TEMPLATE[i][1] - 3.0]
        });
        let m = similarity(&TEMPLATE, &dst);
        for (got, want) in m.iter().zip([a, b, 5.0, -3.0]) {
            assert!((got - want).abs() < 1e-3, "{m:?}");
        }
    }

    #[test]
    fn nms_drops_overlaps() {
        let f = |x: f32, s: f32| Face {
            x,
            y: 0.0,
            w: 10.0,
            h: 10.0,
            score: s,
            landmarks: [[0.0; 2]; 5],
            embedding: vec![],
        };
        let kept = nms(vec![f(0.0, 0.9), f(1.0, 0.8), f(50.0, 0.85)]);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].score, 0.9);
    }

    #[test]
    fn align_maps_landmarks_onto_template() {
        // Draw bright dots at the "landmarks" of a scaled/shifted face and
        // check they land on the template after alignment.
        let lm: [[f32; 2]; 5] = std::array::from_fn(|i| [TEMPLATE[i][0] * 1.5 + 40.0, TEMPLATE[i][1] * 1.5 + 20.0]);
        let mut img = RgbImage::new(300, 300);
        for p in &lm {
            for dy in -1..=1 {
                for dx in -1..=1 {
                    img.put_pixel((p[0] as i32 + dx) as u32, (p[1] as i32 + dy) as u32, image::Rgb([255, 255, 255]));
                }
            }
        }
        let crop = align(&img, &lm);
        for t in TEMPLATE {
            assert!(crop.get_pixel(t[0].round() as u32, t[1].round() as u32).0[0] > 100);
        }
    }
}
