//! CLIP image/text embeddings (TinyCLIP, 512-d, L2-normalised).
//!
//! The ONNX export is a single graph with both towers; one plan with a
//! batch of one image and one text serves both directions (the unused tower
//! gets a dummy input — cheap for the text side, ~0.15 s for images).

use super::tokenizer::{Tokenizer, CONTEXT};
use super::{load_plan, normalize, to_tensor, tract_err, Plan};
use crate::error::Result;
use image::{imageops, RgbImage};
use std::path::Path;
use tract_onnx::prelude::*;

pub const DIM: usize = 512;
const SIZE: u32 = 224;
const MEAN: [f32; 3] = [0.481_454_7, 0.457_827_5, 0.408_210_7];
const STD: [f32; 3] = [0.268_629_5, 0.261_302_6, 0.275_777_1];
/// CLIP's learned temperature (logit scale) for zero-shot probabilities.
pub const LOGIT_SCALE: f32 = 100.0;

pub struct Clip {
    plan: Plan,
    tok: Tokenizer,
}

impl Clip {
    pub fn load(dir: &Path) -> Result<Self> {
        let tok = Tokenizer::new(
            &std::fs::read_to_string(dir.join("vocab.json"))?,
            &std::fs::read_to_string(dir.join("merges.txt"))?,
        )?;
        let plan = load_plan(
            &dir.join("clip.onnx"),
            &[
                (i64::datum_type(), &[1, CONTEXT]),
                (f32::datum_type(), &[1, 3, SIZE as usize, SIZE as usize]),
                (i64::datum_type(), &[1, CONTEXT]),
            ],
        )?;
        Ok(Self { plan, tok })
    }

    /// Resize shortest edge to 224 (bicubic), centre-crop, normalise.
    fn preprocess(img: &RgbImage) -> Tensor {
        let (w, h) = img.dimensions();
        let s = SIZE as f32 / w.min(h).max(1) as f32;
        let (nw, nh) = (((w as f32 * s).round() as u32).max(SIZE), ((h as f32 * s).round() as u32).max(SIZE));
        let resized = imageops::resize(img, nw, nh, imageops::FilterType::CatmullRom);
        let crop = imageops::crop_imm(&resized, (nw - SIZE) / 2, (nh - SIZE) / 2, SIZE, SIZE).to_image();
        to_tensor(&crop, [0, 1, 2], |c, v| (v / 255.0 - MEAN[c]) / STD[c])
    }

    fn run(&self, text: &str, pixels: Tensor) -> Result<(Vec<f32>, Vec<f32>)> {
        let (ids, mask) = self.tok.encode(text);
        let ids = tract_ndarray::Array2::from_shape_vec((1, CONTEXT), ids.to_vec()).expect("shape");
        let mask = tract_ndarray::Array2::from_shape_vec((1, CONTEXT), mask.to_vec()).expect("shape");
        let out = self
            .plan
            .run(tvec![Tensor::from(ids).into(), pixels.into(), Tensor::from(mask).into()])
            .map_err(tract_err)?;
        // Outputs: logits_per_image, logits_per_text, text_embeds, image_embeds.
        let take = |i: usize| -> Result<Vec<f32>> {
            let mut v = out[i].to_plain_array_view::<f32>().map_err(tract_err)?.iter().copied().collect::<Vec<_>>();
            normalize(&mut v);
            Ok(v)
        };
        Ok((take(2)?, take(3)?))
    }

    pub fn embed_image(&self, img: &RgbImage) -> Result<Vec<f32>> {
        Ok(self.run("", Self::preprocess(img))?.1)
    }

    pub fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        let blank = Tensor::zero::<f32>(&[1, 3, SIZE as usize, SIZE as usize]).map_err(tract_err)?;
        Ok(self.run(text, blank)?.0)
    }
}

/// Softmax over `scores` (cosine similarities) with CLIP's logit scale.
pub fn softmax(scores: &[f32]) -> Vec<f32> {
    let max = scores.iter().copied().fold(f32::MIN, f32::max);
    let e: Vec<f32> = scores.iter().map(|s| ((s - max) * LOGIT_SCALE).exp()).collect();
    let sum: f32 = e.iter().sum();
    e.into_iter().map(|v| v / sum).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn softmax_sums_to_one_and_ranks() {
        let p = softmax(&[0.30, 0.25, 0.10]);
        assert!((p.iter().sum::<f32>() - 1.0).abs() < 1e-5);
        assert!(p[0] > p[1] && p[1] > p[2]);
    }

    /// Zero-shot sanity check on synthetic images with the real model
    /// (runs only when PRISM_MODELS_DIR is set).
    #[test]
    fn zero_shot_on_synthetic_scenes() {
        let Ok(dir) = std::env::var("PRISM_MODELS_DIR") else { return };
        let clip = Clip::load(Path::new(&dir)).unwrap();
        let sky = RgbImage::from_fn(320, 240, |_, y| {
            let t = y as f32 / 240.0;
            image::Rgb([(90.0 + 80.0 * t) as u8, (150.0 + 60.0 * t) as u8, 235])
        });
        let grass = RgbImage::from_fn(320, 240, |x, y| {
            let n = ((x * 7 + y * 13) % 17) as u8;
            image::Rgb([40 + n, 120 + n * 3, 30])
        });
        let labels = ["a photo of a clear blue sky", "a photo of green grass"];
        let t: Vec<Vec<f32>> = labels.iter().map(|l| clip.embed_text(l).unwrap()).collect();
        for (img, want) in [(&sky, 0), (&grass, 1)] {
            let e = clip.embed_image(img).unwrap();
            assert_eq!(e.len(), DIM);
            let s: Vec<f32> = t.iter().map(|v| super::super::cosine(&e, v)).collect();
            assert_eq!(s.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0, want, "{s:?}");
        }
    }
}
