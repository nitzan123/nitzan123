//! Salient-subject segmentation with U²-Netp → soft mask (0…1) at the
//! image's aspect ratio.

use super::{load_plan, to_tensor, tract_err, Plan};
use crate::error::Result;
use image::{GrayImage, RgbImage};
use std::path::Path;
use tract_onnx::prelude::*;

const SIZE: usize = 320;
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];

pub struct Segmenter {
    plan: Plan,
}

impl Segmenter {
    pub fn load(dir: &Path) -> Result<Self> {
        Ok(Self { plan: load_plan(&dir.join("u2netp.onnx"), &[(f32::datum_type(), &[1, 3, SIZE, SIZE])])? })
    }

    /// Subject mask with the image's aspect ratio, long edge 320 px.
    pub fn subject(&self, img: &RgbImage) -> Result<GrayImage> {
        let (w, h) = img.dimensions();
        let s = SIZE as f32 / w.max(h).max(1) as f32;
        let (nw, nh) = (
            ((w as f32 * s).round() as u32).clamp(1, SIZE as u32),
            ((h as f32 * s).round() as u32).clamp(1, SIZE as u32),
        );
        let resized = image::imageops::resize(img, nw, nh, image::imageops::FilterType::Triangle);
        let mut canvas = RgbImage::new(SIZE as u32, SIZE as u32);
        image::imageops::replace(&mut canvas, &resized, 0, 0);
        let out = self
            .plan
            .run(tvec![to_tensor(&canvas, [0, 1, 2], |c, v| (v / 255.0 - MEAN[c]) / STD[c]).into()])
            .map_err(tract_err)?;
        let map = out[0].to_plain_array_view::<f32>().map_err(tract_err)?;
        let vals: Vec<f32> = (0..nh as usize)
            .flat_map(|y| (0..nw as usize).map(move |x| (y, x)))
            .map(|(y, x)| map[[0, 0, y, x]])
            .collect();
        let (lo, hi) = vals.iter().fold((f32::MAX, f32::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
        let range = (hi - lo).max(1e-6);
        Ok(GrayImage::from_fn(nw, nh, |x, y| {
            image::Luma([(((vals[(y * nw + x) as usize] - lo) / range) * 255.0).round() as u8])
        }))
    }
}
