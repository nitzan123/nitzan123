//! On-device AI: people (face detection + recognition), smart search and
//! keyword suggestions (CLIP image/text embeddings) and AI masks (salient
//! subject segmentation).
//!
//! Models are downloaded once, on the user's request, from Hugging Face and
//! verified by SHA-256; inference runs locally with `tract` (pure Rust, no
//! native libraries). Photos never leave the computer.
//!
//! | file | model | licence |
//! |------|-------|---------|
//! | yunet.onnx | OpenCV YuNet face detector (2023mar) | MIT |
//! | sface.onnx | OpenCV SFace face recognition (2021dec) | Apache-2.0 |
//! | clip.onnx (+ vocab/merges) | TinyCLIP ViT-40M/32 Text-19M (int8) | MIT |
//! | u2netp.onnx | U²-Netp salient object segmentation | Apache-2.0 |

pub mod clip;
pub mod faces;
pub mod segment;
pub mod tokenizer;
pub mod vocab;

use crate::error::{Error, Result};
use image::RgbImage;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tract_onnx::prelude::*;

pub(crate) type Plan = Arc<TypedSimplePlan>;

pub struct ModelFile {
    pub name: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

const HF: &str = "https://huggingface.co";

pub const MODEL_FILES: &[ModelFile] = &[
    ModelFile {
        name: "yunet.onnx",
        url: "/opencv/face_detection_yunet/resolve/main/face_detection_yunet_2023mar.onnx",
        sha256: "8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4",
        size: 232_589,
    },
    ModelFile {
        name: "sface.onnx",
        url: "/opencv/face_recognition_sface/resolve/main/face_recognition_sface_2021dec.onnx",
        sha256: "0ba9fbfa01b5270c96627c4ef784da859931e02f04419c829e83484087c34e79",
        size: 38_696_353,
    },
    ModelFile {
        name: "u2netp.onnx",
        url: "/BritishWerewolf/U-2-Netp/resolve/main/onnx/model.onnx",
        sha256: "309c8469258dda742793dce0ebea8e6dd393174f89934733ecc8b14c76f4ddd8",
        size: 4_574_861,
    },
    ModelFile {
        name: "clip.onnx",
        url: "/onnx-community/TinyCLIP-ViT-40M-32-Text-19M-LAION400M-ONNX/resolve/main/onnx/model_quantized.onnx",
        sha256: "5e9390262b0eb80caec3eb878acc8b77e116570a2d3b8ea4a59d13029d6f3329",
        size: 85_618_595,
    },
    ModelFile {
        name: "vocab.json",
        url: "/onnx-community/TinyCLIP-ViT-40M-32-Text-19M-LAION400M-ONNX/resolve/main/vocab.json",
        sha256: "5047b556ce86ccaf6aa22b3ffccfc52d391ea4accdab9c2f2407da5b742d4363",
        size: 862_328,
    },
    ModelFile {
        name: "merges.txt",
        url: "/onnx-community/TinyCLIP-ViT-40M-32-Text-19M-LAION400M-ONNX/resolve/main/merges.txt",
        sha256: "9fd691f7c8039210e0fced15865466c65820d09b63988b0174bfe25de299051a",
        size: 524_619,
    },
];

pub fn total_download_size() -> u64 {
    MODEL_FILES.iter().map(|f| f.size).sum()
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}

/// True when every model file is present with the expected size.
pub fn installed(dir: &Path) -> bool {
    MODEL_FILES.iter().all(|f| std::fs::metadata(dir.join(f.name)).is_ok_and(|m| m.len() == f.size))
}

/// Downloads missing/corrupt model files into `dir`, verifying SHA-256.
/// `progress(bytes_done, bytes_total)`.
pub fn download(dir: &Path, mut progress: impl FnMut(u64, u64)) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let agent = ureq::AgentBuilder::new()
        .user_agent(concat!("PrismStudio/", env!("CARGO_PKG_VERSION")))
        .timeout_connect(std::time::Duration::from_secs(30))
        .try_proxy_from_env(true)
        .build();
    let total = total_download_size();
    let mut done = 0u64;
    for f in MODEL_FILES {
        let target = dir.join(f.name);
        if std::fs::metadata(&target).is_ok_and(|m| m.len() == f.size) && sha256_file(&target)? == f.sha256 {
            done += f.size;
            progress(done, total);
            continue;
        }
        let tmp = target.with_extension("part");
        let resp =
            agent.get(&format!("{HF}{}", f.url)).call().map_err(|e| Error::Network(format!("{}: {e}", f.name)))?;
        let mut reader = resp.into_reader();
        let mut out = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
        let mut buf = vec![0u8; 1 << 16];
        let mut last_report = 0u64;
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            std::io::Write::write_all(&mut out, &buf[..n])?;
            done += n as u64;
            if done - last_report > 1 << 20 {
                progress(done, total);
                last_report = done;
            }
        }
        drop(out);
        let got = sha256_file(&tmp)?;
        if got != f.sha256 {
            let _ = std::fs::remove_file(&tmp);
            return Err(Error::Network(format!("{}: checksum mismatch (got {got})", f.name)));
        }
        std::fs::rename(&tmp, &target)?;
        progress(done, total);
    }
    Ok(())
}

pub(crate) fn tract_err(e: impl std::fmt::Display) -> Error {
    Error::NotFound(format!("AI model: {e}"))
}

pub(crate) fn load_plan(path: &Path, inputs: &[(DatumType, &[usize])]) -> Result<Plan> {
    let mut m = tract_onnx::onnx()
        .with_ignore_value_info(true)
        .with_ignore_output_shapes(true)
        .model_for_path(path)
        .map_err(tract_err)?;
    for (i, (dt, shape)) in inputs.iter().enumerate() {
        m.set_input_fact(i, InferenceFact::dt_shape(*dt, shape.to_vec())).map_err(tract_err)?;
    }
    m.into_optimized().map_err(tract_err)?.into_runnable().map_err(tract_err)
}

/// NCHW float tensor from an RGB image: `f(channel, value_0_255)`.
pub(crate) fn to_tensor(img: &RgbImage, channel_order: [usize; 3], f: impl Fn(usize, f32) -> f32) -> Tensor {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let mut data = vec![0f32; 3 * w * h];
    for (x, y, p) in img.enumerate_pixels() {
        for (ci, &src) in channel_order.iter().enumerate() {
            data[ci * w * h + y as usize * w + x as usize] = f(ci, p.0[src] as f32);
        }
    }
    tract_ndarray::Array4::from_shape_vec((1, 3, h, w), data).expect("shape").into()
}

pub fn normalize(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        v.iter_mut().for_each(|x| *x /= n);
    }
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Lazily loaded models (each ~0.1–2 s to prepare).
pub struct Models {
    dir: PathBuf,
    clip: Mutex<Option<Arc<clip::Clip>>>,
    faces: Mutex<Option<Arc<faces::FaceModels>>>,
    segment: Mutex<Option<Arc<segment::Segmenter>>>,
}

impl Models {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into(), clip: Mutex::new(None), faces: Mutex::new(None), segment: Mutex::new(None) }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn installed(&self) -> bool {
        installed(&self.dir)
    }

    fn lazy<T>(&self, slot: &Mutex<Option<Arc<T>>>, load: impl FnOnce(&Path) -> Result<T>) -> Result<Arc<T>> {
        if !self.installed() {
            return Err(Error::NotFound("AI models are not installed".into()));
        }
        let mut g = slot.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(m) = g.as_ref() {
            return Ok(m.clone());
        }
        let m = Arc::new(load(&self.dir)?);
        *g = Some(m.clone());
        Ok(m)
    }

    pub fn clip(&self) -> Result<Arc<clip::Clip>> {
        self.lazy(&self.clip, clip::Clip::load)
    }

    pub fn faces(&self) -> Result<Arc<faces::FaceModels>> {
        self.lazy(&self.faces, faces::FaceModels::load)
    }

    pub fn segmenter(&self) -> Result<Arc<segment::Segmenter>> {
        self.lazy(&self.segment, segment::Segmenter::load)
    }
}

// ------------------------------------------------------------ high level

/// Text embeddings of the keyword vocabulary, cached next to the models.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct LabelIndex {
    pub labels: Vec<String>,
    pub embeddings: Vec<Vec<f32>>,
}

impl LabelIndex {
    pub fn load_or_build(models: &Models) -> Result<Self> {
        let path = models.dir().join("labels.json");
        let want: Vec<String> = vocab::LABELS.iter().map(|s| s.to_string()).collect();
        if let Ok(idx) = std::fs::read_to_string(&path)
            .map_err(Error::from)
            .and_then(|s| Ok(serde_json::from_str::<LabelIndex>(&s)?))
        {
            if idx.labels == want {
                return Ok(idx);
            }
        }
        let clip = models.clip()?;
        use rayon::prelude::*;
        let embeddings = want.par_iter().map(|l| clip.embed_text(&vocab::prompt(l))).collect::<Result<Vec<_>>>()?;
        let idx = LabelIndex { labels: want, embeddings };
        std::fs::write(&path, serde_json::to_string(&idx)?)?;
        Ok(idx)
    }

    /// Zero-shot keyword suggestions: up to 5 labels with probability ≥ 12 %.
    pub fn tags(&self, image_embedding: &[f32]) -> Vec<(String, f32)> {
        let scores: Vec<f32> = self.embeddings.iter().map(|t| cosine(image_embedding, t)).collect();
        let p = clip::softmax(&scores);
        let mut idx: Vec<usize> = (0..p.len()).collect();
        idx.sort_by(|a, b| p[*b].total_cmp(&p[*a]));
        idx.into_iter()
            .take(5)
            .filter(|&i| p[i] >= 0.12)
            .map(|i| (vocab::display(&self.labels[i]).to_string(), p[i]))
            .collect()
    }
}

/// CLIP embedding, keyword suggestions and faces of one image.
pub type ImageAnalysis = (Vec<f32>, Vec<(String, f32)>, Vec<crate::catalog::AnalyzedFace>);

/// Full analysis of one (orientation-corrected) image: CLIP embedding,
/// keyword suggestions and faces with normalised boxes.
pub fn analyze(models: &Models, labels: &LabelIndex, img: &RgbImage) -> Result<ImageAnalysis> {
    let embedding = models.clip()?.embed_image(img)?;
    let tags = labels.tags(&embedding);
    let (w, h) = (img.width() as f32, img.height() as f32);
    let faces = models
        .faces()?
        .analyze(img)?
        .into_iter()
        // Ignore tiny faces (background crowds): < 2 % of the short side.
        .filter(|f| f.w.min(f.h) >= w.min(h) * 0.02)
        .map(|f| crate::catalog::AnalyzedFace {
            rect: [(f.x / w).clamp(0.0, 1.0), (f.y / h).clamp(0.0, 1.0), (f.w / w).min(1.0), (f.h / h).min(1.0)],
            score: f.score,
            embedding: f.embedding,
        })
        .collect();
    Ok((embedding, tags, faces))
}

/// AI mask over the whole (uncropped, orientation-corrected) image.
pub fn mask(models: &Models, img: &RgbImage, kind: &str) -> Result<crate::develop::masks::Mask> {
    let subject = models.segmenter()?.subject(img)?;
    let gray = match kind {
        "subject" => subject,
        "sky" => {
            // Heuristic sky, minus anything the segmenter calls subject.
            let small =
                image::imageops::resize(img, subject.width(), subject.height(), image::imageops::FilterType::Triangle);
            let sky = crate::develop::masks::sky_from_rgb(&small, 50.0);
            image::GrayImage::from_fn(subject.width(), subject.height(), |x, y| {
                let i = (y * subject.width() + x) as usize;
                let s = subject.get_pixel(x, y).0[0] as f32 / 255.0;
                image::Luma([(sky[i] * (1.0 - s).powf(1.5) * 255.0).round() as u8])
            })
        }
        other => return Err(Error::NotFound(format!("unknown AI mask kind {other}"))),
    };
    Ok(crate::develop::masks::Mask::from_gray(kind, &gray))
}

/// Embeddings are stored as little-endian f32 blobs.
pub fn to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

pub fn from_blob(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_roundtrip_and_cosine() {
        let mut v = vec![3.0, 4.0];
        normalize(&mut v);
        assert_eq!(v, vec![0.6, 0.8]);
        assert_eq!(from_blob(&to_blob(&v)), v);
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn manifest_is_consistent() {
        assert!(MODEL_FILES.iter().all(|f| f.sha256.len() == 64 && f.url.starts_with('/')));
        assert!(total_download_size() > 100_000_000);
        assert!(!installed(Path::new("/nonexistent")));
    }
}
