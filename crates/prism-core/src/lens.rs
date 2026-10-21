//! Lens corrections: distortion, transverse chromatic aberration (TCA) and
//! vignetting, either from a lensfun-format profile database or from manual
//! sliders.
//!
//! The database (lensfun, CC BY-SA 3.0) is not bundled; the user downloads
//! it on demand (see `src-tauri`). A photo's profile is resolved once —
//! matched by EXIF lens/camera and interpolated for its focal length and
//! aperture — and the resulting coefficients are stored in the develop
//! settings, so rendering never needs the database.
//!
//! Units follow lensfun/Hugin: distortion and TCA radii are 1 at half the
//! calibration frame's short side, vignetting radii are 1 at its corner;
//! both are rescaled for the camera's crop factor. Formulas (corrected
//! position → source position):
//! * poly3:  Rd = Ru (1 − k1 + k1 Ru²)
//! * poly5:  Rd = Ru (1 + k1 Ru² + k2 Ru⁴)
//! * ptlens: Rd = Ru (a Ru³ + b Ru² + c Ru + 1 − a − b − c)
//! * TCA linear: R′ = kr·R (red), kb·R (blue); poly3: R′ = R (b R² + c R + v)
//! * vignetting pa: V = 1 + k1 r² + k2 r⁴ + k3 r⁶, corrected = pixel / V

use crate::develop::buffer::Buf;
use crate::error::{Error, Result};
use quick_xml::events::{BytesStart, Event};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::Path;

// ----------------------------------------------------------------- settings

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "model", rename_all = "camelCase")]
pub enum Distortion {
    Poly3 { k1: f32 },
    Poly5 { k1: f32, k2: f32 },
    Ptlens { a: f32, b: f32, c: f32 },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "model", rename_all = "camelCase")]
pub enum Tca {
    Linear { kr: f32, kb: f32 },
    Poly3 { vr: f32, vb: f32, cr: f32, cb: f32, br: f32, bb: f32 },
}

/// Coefficients resolved for one photo (focal length, aperture, camera).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LensProfile {
    pub lens: String,
    pub focal: f32,
    /// Crop factor and aspect ratio of the calibration frame.
    pub calib_crop: f32,
    pub calib_aspect: f32,
    /// Crop factor of the camera the photo was taken with.
    pub camera_crop: f32,
    pub distortion: Option<Distortion>,
    pub tca: Option<Tca>,
    /// pa model `[k1, k2, k3]`.
    pub vignetting: Option<[f32; 3]>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct LensSettings {
    /// Apply `profile`.
    pub enabled: bool,
    pub profile: Option<LensProfile>,
    /// Profile strengths, 0 … 200 %.
    pub distortion: f32,
    pub vignetting: f32,
    pub ca: f32,
    /// Manual corrections, applied on top. -100 … 100.
    pub manual_distortion: f32,
    pub manual_vignetting: f32,
    /// Red/blue channel scale, -100 … 100 (±0.2 %).
    pub manual_ca_red: f32,
    pub manual_ca_blue: f32,
}

impl Default for LensSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            profile: None,
            distortion: 100.0,
            vignetting: 100.0,
            ca: 100.0,
            manual_distortion: 0.0,
            manual_vignetting: 0.0,
            manual_ca_red: 0.0,
            manual_ca_blue: 0.0,
        }
    }
}

impl LensSettings {
    fn profile(&self) -> Option<&LensProfile> {
        self.profile.as_ref().filter(|_| self.enabled)
    }
}

// ------------------------------------------------------------------ apply

impl Distortion {
    /// Source radius for corrected radius `ru` (Hugin units).
    fn rd(&self, ru: f32) -> f32 {
        let r2 = ru * ru;
        match *self {
            Distortion::Poly3 { k1 } => ru * (1.0 - k1 + k1 * r2),
            Distortion::Poly5 { k1, k2 } => ru * (1.0 + k1 * r2 + k2 * r2 * r2),
            Distortion::Ptlens { a, b, c } => ru * (a * r2 * ru + b * r2 + c * ru + 1.0 - a - b - c),
        }
    }
}

impl Tca {
    /// Radius scale factors (red, blue) at distorted radius `r`.
    fn scales(&self, r: f32) -> (f32, f32) {
        match *self {
            Tca::Linear { kr, kb } => (kr, kb),
            Tca::Poly3 { vr, vb, cr, cb, br, bb } => (br * r * r + cr * r + vr, bb * r * r + cb * r + vb),
        }
    }
}

/// Applies lens corrections to a linear image (before any geometry edits).
pub(crate) fn apply(b: Buf, s: &LensSettings) -> Buf {
    let profile = s.profile();
    let (w, h) = (b.w as f32, b.h as f32);
    let diag = (w * w + h * h).sqrt();
    let (cx, cy) = ((w - 1.0) / 2.0, (h - 1.0) / 2.0);
    let crop_ratio = profile.map_or(1.0, |p| p.calib_crop / p.camera_crop.max(0.1));
    // Pixel → Hugin radius units.
    let unit_h = match profile {
        Some(p) => 2.0 * (p.calib_aspect * p.calib_aspect + 1.0).sqrt() / diag * crop_ratio,
        None => 2.0 / w.min(h),
    };
    let unit_v = 2.0 / diag * crop_ratio;

    let mut b = b;
    // 1. Vignetting, evaluated at source positions.
    let vig = profile.and_then(|p| p.vignetting).filter(|_| s.vignetting.abs() > 0.5);
    let manual_v = s.manual_vignetting / 100.0 * 0.6;
    if vig.is_some() || manual_v.abs() > 1e-4 {
        let strength = s.vignetting / 100.0;
        let bw = b.w;
        b.px.par_chunks_mut(bw).enumerate().for_each(|(y, row)| {
            let dy = (y as f32 - cy) * unit_v;
            for (x, p) in row.iter_mut().enumerate() {
                let dx = (x as f32 - cx) * unit_v;
                let r2 = dx * dx + dy * dy;
                let mut v = 1.0;
                if let Some([k1, k2, k3]) = vig {
                    let c = 1.0 + k1 * r2 + k2 * r2 * r2 + k3 * r2 * r2 * r2;
                    v *= 1.0 + strength * (c - 1.0);
                }
                // Manual: positive amount brightens corners.
                v *= 1.0 - manual_v * r2.min(1.0);
                let g = 1.0 / v.max(0.2);
                for c in p.iter_mut() {
                    *c *= g;
                }
            }
        });
    }

    // 2. Geometry: distortion + TCA (+ manual), with auto-fit zoom so no
    // empty borders appear.
    let dist = profile.and_then(|p| p.distortion.clone()).filter(|_| s.distortion.abs() > 0.5);
    let tca = profile.and_then(|p| p.tca.clone()).filter(|_| s.ca.abs() > 0.5);
    let manual_k1 = -s.manual_distortion / 100.0 * 0.15;
    let manual_r = 1.0 + s.manual_ca_red / 100.0 * 0.002;
    let manual_b = 1.0 + s.manual_ca_blue / 100.0 * 0.002;
    let geometric = dist.is_some() || tca.is_some() || manual_k1.abs() > 1e-5 || manual_r != 1.0 || manual_b != 1.0;
    if !geometric {
        return b;
    }
    let (sd, sca) = (s.distortion / 100.0, s.ca / 100.0);
    let map = |ux: f32, uy: f32| -> [(f32, f32); 3] {
        let ru = (ux * ux + uy * uy).sqrt();
        let mut rd = ru;
        if let Some(d) = &dist {
            rd = ru + sd * (d.rd(ru) - ru);
        }
        rd *= 1.0 - manual_k1 + manual_k1 * rd * rd;
        let k = if ru > 1e-9 { rd / ru } else { 1.0 };
        let (gx, gy) = (ux * k, uy * k);
        let (mut tr, mut tb) = tca.as_ref().map_or((1.0, 1.0), |t| t.scales(rd));
        tr = 1.0 + sca * (tr - 1.0);
        tb = 1.0 + sca * (tb - 1.0);
        tr *= manual_r;
        tb *= manual_b;
        let to_px = |x: f32, y: f32| (x / unit_h + cx, y / unit_h + cy);
        [to_px(gx * tr, gy * tr), to_px(gx, gy), to_px(gx * tb, gy * tb)]
    };
    // Largest zoom factor ≤ 1 that keeps the frame's border inside the source.
    let border = [
        (0.0, 0.0),
        (w - 1.0, 0.0),
        (0.0, h - 1.0),
        (w - 1.0, h - 1.0),
        (cx, 0.0),
        (cx, h - 1.0),
        (0.0, cy),
        (w - 1.0, cy),
    ];
    let fits = |z: f32| {
        border.iter().all(|&(x, y)| {
            map((x - cx) * unit_h * z, (y - cy) * unit_h * z)
                .iter()
                .all(|&(sx, sy)| sx >= -0.5 && sy >= -0.5 && sx <= w - 0.5 && sy <= h - 0.5)
        })
    };
    let zoom = if fits(1.0) {
        1.0
    } else {
        let (mut lo, mut hi) = (0.3f32, 1.0f32);
        for _ in 0..24 {
            let mid = (lo + hi) / 2.0;
            if fits(mid) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    };
    let mut out = Buf::new(b.w, b.h);
    let bw = b.w;
    out.px.par_chunks_mut(bw).enumerate().for_each(|(y, row)| {
        let uy = (y as f32 - cy) * unit_h * zoom;
        for (x, o) in row.iter_mut().enumerate() {
            let ux = (x as f32 - cx) * unit_h * zoom;
            let m = map(ux, uy);
            for c in 0..3 {
                o[c] = b.sample(m[c].0, m[c].1)[c];
            }
        }
    });
    out
}

// ---------------------------------------------------------------- database

#[derive(Debug, Clone, Default)]
pub struct LensEntry {
    pub maker: String,
    pub models: Vec<String>,
    pub mount: String,
    pub crop: f32,
    pub aspect: f32,
    pub distortion: Vec<(f32, Distortion)>,
    pub tca: Vec<(f32, Tca)>,
    /// (focal, aperture, distance, [k1, k2, k3])
    pub vignetting: Vec<(f32, f32, f32, [f32; 3])>,
}

#[derive(Debug, Clone, Default)]
pub struct CameraEntry {
    pub maker: String,
    pub models: Vec<String>,
    pub crop: f32,
}

#[derive(Debug, Default)]
pub struct LensDb {
    pub lenses: Vec<LensEntry>,
    pub cameras: Vec<CameraEntry>,
}

/// Lower-case alphanumerics only: "EF85mm f/1.8 USM" ≈ "EF 85mm f/1.8 USM".
fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Lens model strings often omit the maker prefix ("EF85mm…" vs
/// "Canon EF 85mm…"); compare without it.
fn strip_maker(model: &str, maker: &str) -> String {
    let (m, k) = (norm(model), norm(maker.split_whitespace().next().unwrap_or("")));
    m.strip_prefix(&k).map(str::to_string).unwrap_or(m)
}

impl LensDb {
    /// Loads every `*.xml` file in `dir`.
    pub fn load_dir(dir: &Path) -> Result<Self> {
        let mut db = LensDb::default();
        for entry in std::fs::read_dir(dir)? {
            let p = entry?.path();
            if p.extension().is_some_and(|e| e == "xml") {
                db.parse_into(&std::fs::read_to_string(&p)?)?;
            }
        }
        Ok(db)
    }

    pub fn parse(xml: &str) -> Result<Self> {
        let mut db = LensDb::default();
        db.parse_into(xml)?;
        Ok(db)
    }

    fn parse_into(&mut self, xml: &str) -> Result<()> {
        let mut r = quick_xml::Reader::from_str(xml);
        r.config_mut().trim_text(true);
        let bad = |e: quick_xml::Error| Error::Network(format!("lens database: {e}"));
        let attr = |e: &BytesStart, k: &str| -> Option<f32> {
            e.attributes()
                .flatten()
                .find(|a| a.key.as_ref() == k.as_bytes())
                .and_then(|a| a.unescape_value().ok().and_then(|v| v.trim().parse().ok()))
        };
        let attr_s = |e: &BytesStart, k: &str| -> Option<String> {
            e.attributes()
                .flatten()
                .find(|a| a.key.as_ref() == k.as_bytes())
                .and_then(|a| a.unescape_value().ok().map(|v| v.to_string()))
        };
        enum Ctx {
            None,
            Lens(LensEntry),
            Camera(CameraEntry),
        }
        let mut ctx = Ctx::None;
        let mut field: Option<String> = None;
        loop {
            match r.read_event().map_err(bad)? {
                Event::Eof => break,
                Event::Start(e) => {
                    let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    match (name.as_str(), &mut ctx) {
                        ("lens", _) => ctx = Ctx::Lens(LensEntry { crop: 1.0, aspect: 1.5, ..Default::default() }),
                        ("camera", _) => ctx = Ctx::Camera(CameraEntry { crop: 1.0, ..Default::default() }),
                        _ => field = Some(name),
                    }
                }
                Event::Empty(e) => {
                    if let Ctx::Lens(l) = &mut ctx {
                        let focal = attr(&e, "focal").unwrap_or(0.0);
                        let model = attr_s(&e, "model").unwrap_or_default();
                        match e.name().as_ref() {
                            b"distortion" => {
                                let d = match model.as_str() {
                                    "poly3" => Some(Distortion::Poly3 { k1: attr(&e, "k1").unwrap_or(0.0) }),
                                    "poly5" => Some(Distortion::Poly5 {
                                        k1: attr(&e, "k1").unwrap_or(0.0),
                                        k2: attr(&e, "k2").unwrap_or(0.0),
                                    }),
                                    "ptlens" => Some(Distortion::Ptlens {
                                        a: attr(&e, "a").unwrap_or(0.0),
                                        b: attr(&e, "b").unwrap_or(0.0),
                                        c: attr(&e, "c").unwrap_or(0.0),
                                    }),
                                    _ => None,
                                };
                                if let Some(d) = d {
                                    l.distortion.push((focal, d));
                                }
                            }
                            b"tca" => {
                                let t = match model.as_str() {
                                    "linear" => Some(Tca::Linear {
                                        kr: attr(&e, "kr").unwrap_or(1.0),
                                        kb: attr(&e, "kb").unwrap_or(1.0),
                                    }),
                                    "poly3" => Some(Tca::Poly3 {
                                        vr: attr(&e, "vr").unwrap_or(1.0),
                                        vb: attr(&e, "vb").unwrap_or(1.0),
                                        cr: attr(&e, "cr").unwrap_or(0.0),
                                        cb: attr(&e, "cb").unwrap_or(0.0),
                                        br: attr(&e, "br").unwrap_or(0.0),
                                        bb: attr(&e, "bb").unwrap_or(0.0),
                                    }),
                                    _ => None,
                                };
                                if let Some(t) = t {
                                    l.tca.push((focal, t));
                                }
                            }
                            b"vignetting" if model == "pa" => l.vignetting.push((
                                focal,
                                attr(&e, "aperture").unwrap_or(8.0),
                                attr(&e, "distance").unwrap_or(1000.0),
                                [
                                    attr(&e, "k1").unwrap_or(0.0),
                                    attr(&e, "k2").unwrap_or(0.0),
                                    attr(&e, "k3").unwrap_or(0.0),
                                ],
                            )),
                            _ => {}
                        }
                    }
                }
                Event::Text(t) => {
                    let text = t.unescape().map_err(bad)?.trim().to_string();
                    match (field.as_deref(), &mut ctx) {
                        (Some("maker"), Ctx::Lens(l)) if l.maker.is_empty() => l.maker = text,
                        (Some("model"), Ctx::Lens(l)) => l.models.push(text),
                        (Some("mount"), Ctx::Lens(l)) if l.mount.is_empty() => l.mount = text,
                        (Some("cropfactor"), Ctx::Lens(l)) => l.crop = text.parse().unwrap_or(1.0),
                        (Some("aspect-ratio"), Ctx::Lens(l)) => l.aspect = parse_aspect(&text).unwrap_or(1.5),
                        (Some("maker"), Ctx::Camera(c)) if c.maker.is_empty() => c.maker = text,
                        (Some("model"), Ctx::Camera(c)) => c.models.push(text),
                        (Some("cropfactor"), Ctx::Camera(c)) => c.crop = text.parse().unwrap_or(1.0),
                        _ => {}
                    }
                }
                Event::End(e) => match e.name().as_ref() {
                    b"lens" => {
                        if let Ctx::Lens(l) = std::mem::replace(&mut ctx, Ctx::None) {
                            self.lenses.push(l);
                        }
                    }
                    b"camera" => {
                        if let Ctx::Camera(c) = std::mem::replace(&mut ctx, Ctx::None) {
                            self.cameras.push(c);
                        }
                    }
                    _ => field = None,
                },
                _ => {}
            }
        }
        Ok(())
    }

    pub fn camera_crop(&self, make: &str, model: &str) -> Option<f32> {
        let m = strip_maker(model, make);
        let mk = norm(make.split_whitespace().next().unwrap_or(""));
        self.cameras
            .iter()
            .find(|c| norm(&c.maker).starts_with(&mk) && c.models.iter().any(|x| strip_maker(x, &c.maker) == m))
            .map(|c| c.crop)
    }

    /// Lens entries matching an EXIF LensModel, best calibration first.
    pub fn find_lens(&self, lens_model: &str, camera_crop: f32) -> Option<&LensEntry> {
        let target = norm(lens_model);
        if target.len() < 4 {
            return None;
        }
        let matches = |l: &LensEntry| {
            l.models.iter().any(|m| {
                let full = norm(m);
                let bare = strip_maker(m, &l.maker);
                full == target || bare == target || bare == strip_maker(lens_model, &l.maker)
            })
        };
        // lensfun rule: usable if camera_crop / calib_crop ≥ 0.96; prefer the closest.
        self.lenses
            .iter()
            .filter(|l| matches(l) && camera_crop / l.crop >= 0.96)
            .min_by(|a, b| (camera_crop / a.crop).total_cmp(&(camera_crop / b.crop)))
    }
}

/// Database files in the lensfun repository (`data/db/`).
pub const DB_FILES: &[&str] = &[
    "6x6",
    "actioncams",
    "compact-canon",
    "compact-casio",
    "compact-fujifilm",
    "compact-kodak",
    "compact-konica-minolta",
    "compact-leica",
    "compact-nikon",
    "compact-olympus",
    "compact-panasonic",
    "compact-pentax",
    "compact-ricoh",
    "compact-samsung",
    "compact-sigma",
    "compact-sony",
    "contax",
    "generic",
    "mil-canon",
    "mil-fujifilm",
    "mil-hasselblad",
    "mil-leica",
    "mil-nikon",
    "mil-olympus",
    "mil-panasonic",
    "mil-pentax",
    "mil-samsung",
    "mil-samyang",
    "mil-sigma",
    "mil-sony",
    "mil-tamron",
    "mil-tokina",
    "mil-zeiss",
    "misc",
    "om-system",
    "rf-leica",
    "slr-canon",
    "slr-hasselblad",
    "slr-konica-minolta",
    "slr-leica",
    "slr-nikon",
    "slr-olympus",
    "slr-panasonic",
    "slr-pentax",
    "slr-ricoh",
    "slr-samsung",
    "slr-samyang",
    "slr-schneider",
    "slr-sigma",
    "slr-soligor",
    "slr-sony",
    "slr-tamron",
    "slr-tokina",
    "slr-ussr",
    "slr-vivitar",
    "slr-zeiss",
];
pub const DB_BASE_URL: &str = "https://raw.githubusercontent.com/lensfun/lensfun/master/data/db/";

/// Downloads the lensfun database into `dir` (atomically: a temporary
/// folder is filled, validated by parsing, then swapped in).
pub fn download_db(dir: &Path, mut progress: impl FnMut(usize, usize)) -> Result<LensDb> {
    let agent = ureq::AgentBuilder::new()
        .user_agent(concat!("PrismStudio/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(60))
        .try_proxy_from_env(true)
        .build();
    let tmp = dir.with_extension("download");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    for (i, name) in DB_FILES.iter().enumerate() {
        let body = agent
            .get(&format!("{DB_BASE_URL}{name}.xml"))
            .call()
            .map_err(|e| Error::Network(format!("{name}.xml: {e}")))?
            .into_string()?;
        std::fs::write(tmp.join(format!("{name}.xml")), body)?;
        progress(i + 1, DB_FILES.len());
    }
    let db = LensDb::load_dir(&tmp)?;
    if db.lenses.len() < 100 {
        return Err(Error::Network("lens database download looks incomplete".into()));
    }
    let _ = std::fs::remove_dir_all(dir);
    std::fs::rename(&tmp, dir)?;
    Ok(db)
}

fn parse_aspect(s: &str) -> Option<f32> {
    match s.split_once(':') {
        Some((a, b)) => Some(a.trim().parse::<f32>().ok()? / b.trim().parse::<f32>().ok()?),
        None => s.trim().parse().ok(),
    }
}

/// Linear interpolation by focal length between the nearest calibrations
/// below and above (or the nearest one when outside the range).
fn interp_focal<T: Clone>(entries: &[(f32, T)], focal: f32, lerp: impl Fn(&T, &T, f32) -> Option<T>) -> Option<T> {
    let below = entries.iter().filter(|(f, _)| *f <= focal).max_by(|a, b| a.0.total_cmp(&b.0));
    let above = entries.iter().filter(|(f, _)| *f >= focal).min_by(|a, b| a.0.total_cmp(&b.0));
    match (below, above) {
        (Some(a), Some(b)) if (b.0 - a.0).abs() > 1e-3 => {
            lerp(&a.1, &b.1, (focal - a.0) / (b.0 - a.0)).or_else(|| Some(a.1.clone()))
        }
        (Some(a), _) | (None, Some(a)) => Some(a.1.clone()),
        _ => None,
    }
}

impl LensEntry {
    pub fn name(&self) -> String {
        self.models.first().cloned().unwrap_or_default()
    }

    /// Profile for a photo taken at `focal` mm and f/`aperture`.
    pub fn profile(&self, focal: f32, aperture: f32, camera_crop: f32) -> LensProfile {
        let l = |a: f32, b: f32, t: f32| a + (b - a) * t;
        let distortion = interp_focal(&self.distortion, focal, |a, b, t| match (a, b) {
            (Distortion::Poly3 { k1: a }, Distortion::Poly3 { k1: b }) => Some(Distortion::Poly3 { k1: l(*a, *b, t) }),
            (Distortion::Poly5 { k1: a1, k2: a2 }, Distortion::Poly5 { k1: b1, k2: b2 }) => {
                Some(Distortion::Poly5 { k1: l(*a1, *b1, t), k2: l(*a2, *b2, t) })
            }
            (Distortion::Ptlens { a: a1, b: b1, c: c1 }, Distortion::Ptlens { a: a2, b: b2, c: c2 }) => {
                Some(Distortion::Ptlens { a: l(*a1, *a2, t), b: l(*b1, *b2, t), c: l(*c1, *c2, t) })
            }
            _ => None,
        });
        let tca = interp_focal(&self.tca, focal, |a, b, t| match (a, b) {
            (Tca::Linear { kr: r1, kb: b1 }, Tca::Linear { kr: r2, kb: b2 }) => {
                Some(Tca::Linear { kr: l(*r1, *r2, t), kb: l(*b1, *b2, t) })
            }
            (
                Tca::Poly3 { vr, vb, cr, cb, br, bb },
                Tca::Poly3 { vr: vr2, vb: vb2, cr: cr2, cb: cb2, br: br2, bb: bb2 },
            ) => Some(Tca::Poly3 {
                vr: l(*vr, *vr2, t),
                vb: l(*vb, *vb2, t),
                cr: l(*cr, *cr2, t),
                cb: l(*cb, *cb2, t),
                br: l(*br, *br2, t),
                bb: l(*bb, *bb2, t),
            }),
            _ => None,
        });
        LensProfile {
            lens: self.name(),
            focal,
            calib_crop: self.crop,
            calib_aspect: self.aspect,
            camera_crop,
            distortion,
            tca,
            vignetting: self.vignetting_at(focal, aperture),
        }
    }

    /// lensfun's inverse-distance weighting (power 3.5) over focal (relative
    /// to the zoom range), 4/aperture and 0.1/distance; focus distance is
    /// unknown so "far" (1000 m) is assumed.
    fn vignetting_at(&self, focal: f32, aperture: f32) -> Option<[f32; 3]> {
        if self.vignetting.is_empty() {
            return None;
        }
        let (fmin, fmax) = self.vignetting.iter().fold((f32::MAX, f32::MIN), |(lo, hi), v| (lo.min(v.0), hi.max(v.0)));
        let span = (fmax - fmin).max(1e-3);
        let aperture = if aperture > 0.0 { aperture } else { 8.0 };
        let (mut acc, mut wsum) = ([0f32; 3], 0f32);
        for &(f, a, d, k) in &self.vignetting {
            let df = (focal - f) / span;
            let da = 4.0 / aperture - 4.0 / a.max(0.1);
            let dd = 0.1 / 1000.0 - 0.1 / d.max(0.01);
            let dist = (df * df + da * da + dd * dd).sqrt();
            if dist < 1e-4 {
                return Some(k);
            }
            let w = 1.0 / dist.powf(3.5);
            for i in 0..3 {
                acc[i] += w * k[i];
            }
            wsum += w;
        }
        Some(acc.map(|v| v / wsum))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<lensdatabase version="2">
      <camera><maker>Canon</maker><model>Canon EOS 6D</model><mount>Canon EF</mount><cropfactor>1</cropfactor></camera>
      <camera><maker>Canon</maker><model>Canon EOS 80D</model><cropfactor>1.6</cropfactor></camera>
      <lens>
        <maker>Canon</maker>
        <model>Canon EF 85mm f/1.8 USM</model>
        <mount>Canon EF</mount>
        <cropfactor>1</cropfactor>
        <calibration>
          <distortion model="ptlens" focal="85" a="-0.000989" b="-0.002335" c="0"/>
          <tca model="poly3" focal="85" br="-0.0000287" vr="1.0001408" bb="0.0000182" vb="0.9998642"/>
          <vignetting model="pa" focal="85" aperture="1.8" distance="1000" k1="-1.4465" k2="1.4135" k3="-0.6464"/>
          <vignetting model="pa" focal="85" aperture="5" distance="1000" k1="-0.1769" k2="0.5484" k3="-0.5646"/>
        </calibration>
      </lens>
      <lens>
        <maker>Canon</maker>
        <model>Canon EF-S 10-22mm f/3.5-4.5 USM</model>
        <cropfactor>1.613</cropfactor>
        <calibration>
          <distortion model="ptlens" focal="10" a="0.01986" b="-0.06874" c="0.05166"/>
          <distortion model="ptlens" focal="22" a="0.0" b="-0.01" c="0.0"/>
        </calibration>
      </lens>
    </lensdatabase>"#;

    #[test]
    fn parses_and_matches_exif_names() {
        let db = LensDb::parse(XML).unwrap();
        assert_eq!((db.lenses.len(), db.cameras.len()), (2, 2));
        assert_eq!(db.camera_crop("Canon", "Canon EOS 6D"), Some(1.0));
        assert_eq!(db.camera_crop("Canon", "Canon EOS 80D"), Some(1.6));
        // EXIF writes "EF85mm f/1.8 USM" (no maker, no space).
        let l = db.find_lens("EF85mm f/1.8 USM", 1.0).expect("match");
        assert_eq!(l.name(), "Canon EF 85mm f/1.8 USM");
        // A crop-sensor lens can't be used on full frame (lensfun's 0.96 rule).
        assert!(db.find_lens("EF-S10-22mm f/3.5-4.5 USM", 1.0).is_none());
        assert!(db.find_lens("EF-S10-22mm f/3.5-4.5 USM", 1.6).is_some());
        assert!(db.find_lens("Nikon 50mm", 1.0).is_none());
    }

    #[test]
    fn interpolates_by_focal_and_aperture() {
        let db = LensDb::parse(XML).unwrap();
        let wide = db.find_lens("EF-S10-22mm f/3.5-4.5 USM", 1.6).unwrap();
        let p = wide.profile(16.0, 4.0, 1.6);
        let Some(Distortion::Ptlens { b, .. }) = p.distortion else { panic!() };
        assert!((b - (-0.06874 + (-0.01 + 0.06874) * 0.5)).abs() < 1e-5);
        let tele = db.find_lens("EF85mm f/1.8 USM", 1.0).unwrap();
        assert_eq!(tele.profile(85.0, 1.8, 1.0).vignetting, Some([-1.4465, 1.4135, -0.6464]));
        let mid = tele.profile(85.0, 2.8, 1.0).vignetting.unwrap();
        assert!(mid[0] < -0.1769 && mid[0] > -1.4465);
    }

    fn grid(w: usize, h: usize) -> Buf {
        let mut b = Buf::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = if (x / 8 + y / 8) % 2 == 0 { 0.8 } else { 0.1 };
                b.px[y * w + x] = [v, v, v];
            }
        }
        b
    }

    #[test]
    fn identity_settings_change_nothing() {
        let b = grid(64, 48);
        let out = apply(b.clone(), &LensSettings::default());
        assert_eq!(out.px, b.px);
    }

    #[test]
    fn barrel_correction_zooms_to_avoid_empty_borders() {
        let s = LensSettings { manual_distortion: 80.0, ..Default::default() };
        let out = apply(grid(120, 80), &s);
        assert_eq!((out.w, out.h), (120, 80));
        // No black/empty pixels anywhere: every sample came from inside.
        assert!(out.px.iter().all(|p| p[0] >= 0.09));
    }

    #[test]
    fn vignetting_correction_brightens_corners_only() {
        let profile = LensProfile {
            lens: "x".into(),
            focal: 50.0,
            calib_crop: 1.0,
            calib_aspect: 1.5,
            camera_crop: 1.0,
            distortion: None,
            tca: None,
            vignetting: Some([-0.5, 0.0, 0.0]),
        };
        let s = LensSettings { enabled: true, profile: Some(profile), ..Default::default() };
        let mut flat = Buf::new(90, 60);
        flat.px.iter_mut().for_each(|p| *p = [0.3; 3]);
        let out = apply(flat, &s);
        let centre = out.px[30 * 90 + 45][0];
        let corner = out.px[0][0];
        assert!((centre - 0.3).abs() < 0.01, "centre {centre}");
        assert!(corner > 0.5, "corner {corner}");
    }

    #[test]
    fn tca_shifts_red_and_blue_differently() {
        let s = LensSettings { manual_ca_red: 100.0, manual_ca_blue: -100.0, ..Default::default() };
        let out = apply(grid(200, 200), &s);
        // Near an edge far from centre the channels no longer coincide.
        assert!(out.px.iter().any(|p| (p[0] - p[2]).abs() > 0.05));
    }
}
