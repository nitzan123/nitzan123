//! Batch workflow: develop presets, file-name templates (batch rename and
//! export naming) and export encoders.

use crate::develop::{ColorMixer, DevelopSettings, SplitTone, ToneCurve};
use crate::error::{Error, Result};
use crate::Asset;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

// ------------------------------------------------------------------ presets

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Preset {
    /// `None` for built-in presets.
    pub id: Option<i64>,
    pub name: String,
    pub settings: DevelopSettings,
}

/// Applies a preset's *look* (tone, colour, detail, effects) to a photo.
/// Per-photo work — crop/rotation, lens correction, local masks and
/// retouch spots — is
/// kept, so one preset can be applied to a whole shoot.
pub fn apply_preset(base: &DevelopSettings, preset: &DevelopSettings) -> DevelopSettings {
    DevelopSettings {
        geometry: base.geometry.clone(),
        lens: base.lens.clone(),
        local: base.local.clone(),
        spots: base.spots.clone(),
        text: base.text.clone(),
        ..preset.clone()
    }
}

/// The look part of `s` (what [`apply_preset`] transfers), for saving.
pub fn look_of(s: &DevelopSettings) -> DevelopSettings {
    apply_preset(&DevelopSettings::default(), s)
}

/// Presets that ship with the app.
pub fn builtin_presets() -> Vec<Preset> {
    let mk = |name: &str, f: &dyn Fn(&mut DevelopSettings)| {
        let mut s = DevelopSettings::default();
        f(&mut s);
        Preset { id: None, name: name.into(), settings: s }
    };
    vec![
        mk("Vivid", &|s| {
            s.light.contrast = 15.0;
            s.light.clarity = 10.0;
            s.color.vibrance = 35.0;
            s.color.saturation = 8.0;
        }),
        mk("Soft portrait", &|s| {
            s.light.contrast = -10.0;
            s.light.clarity = -15.0;
            s.light.shadows = 15.0;
            s.white_balance.temperature = 6.0;
            s.mixer = ColorMixer { saturation: [-8.0, -10.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], ..Default::default() };
        }),
        mk("Black & white", &|s| {
            s.color.saturation = -100.0;
            s.light.contrast = 20.0;
            s.light.clarity = 10.0;
        }),
        mk("B&W high contrast", &|s| {
            s.color.saturation = -100.0;
            s.light.contrast = 45.0;
            s.light.whites = 15.0;
            s.light.blacks = -20.0;
            s.light.clarity = 25.0;
        }),
        mk("Matte", &|s| {
            s.curve = ToneCurve { points: vec![[0.0, 0.08], [0.25, 0.27], [0.75, 0.76], [1.0, 0.96]] };
            s.color.saturation = -12.0;
        }),
        mk("Warm film", &|s| {
            s.white_balance.temperature = 18.0;
            s.light.contrast = 10.0;
            s.split_tone = SplitTone {
                highlight_hue: 45.0,
                highlight_saturation: 20.0,
                shadow_hue: 200.0,
                shadow_saturation: 12.0,
                balance: 0.0,
            };
            s.vignette.amount = -15.0;
        }),
        mk("Cool", &|s| {
            s.white_balance.temperature = -15.0;
            s.light.contrast = 8.0;
        }),
        mk("Landscape pop", &|s| {
            s.light.highlights = -30.0;
            s.light.shadows = 25.0;
            s.light.clarity = 20.0;
            s.color.vibrance = 30.0;
            s.mixer = ColorMixer {
                saturation: [0.0, 0.0, 0.0, 10.0, 0.0, 15.0, 0.0, 0.0],
                luminance: [0.0, 0.0, 0.0, 0.0, 0.0, -15.0, 0.0, 0.0],
                ..Default::default()
            };
            s.detail.sharpen_amount = 40.0;
        }),
    ]
}

// ---------------------------------------------------------- name templates

/// A file-name template such as `{date}_{name}` or `Trip-{seq:3}`.
///
/// Tokens: `{name}` original name without extension, `{seq}` / `{seq:N}`
/// running number (zero-padded to N digits), `{date}` YYYY-MM-DD,
/// `{time}` HHMMSS, `{year}`, `{month}`, `{day}`, `{camera}`, `{rating}`.
/// Date tokens use the capture date, or the file's modified date (UTC)
/// when the photo has none.
#[derive(Debug, Clone, PartialEq)]
pub struct NameTemplate(Vec<Part>);

#[derive(Debug, Clone, PartialEq)]
enum Part {
    Text(String),
    Name,
    Seq(usize),
    Date,
    Time,
    Year,
    Month,
    Day,
    Camera,
    Rating,
}

impl NameTemplate {
    pub fn parse(s: &str) -> Result<Self> {
        let mut parts = Vec::new();
        let mut rest = s;
        while let Some(open) = rest.find('{') {
            if open > 0 {
                parts.push(Part::Text(rest[..open].into()));
            }
            let close = rest[open..].find('}').ok_or_else(|| Error::Invalid("missing '}' in template".into()))? + open;
            let token = &rest[open + 1..close];
            let (key, arg) = token.split_once(':').unwrap_or((token, ""));
            parts.push(match key.trim().to_ascii_lowercase().as_str() {
                "name" => Part::Name,
                "seq" | "n" => {
                    let w = if arg.is_empty() { 1 } else { arg.parse().map_err(|_| bad(token))? };
                    Part::Seq(w.clamp(1, 9))
                }
                "date" => Part::Date,
                "time" => Part::Time,
                "year" => Part::Year,
                "month" => Part::Month,
                "day" => Part::Day,
                "camera" => Part::Camera,
                "rating" => Part::Rating,
                _ => return Err(bad(token)),
            });
            rest = &rest[close + 1..];
        }
        if !rest.is_empty() {
            parts.push(Part::Text(rest.into()));
        }
        if parts.is_empty() {
            return Err(Error::Invalid("template is empty".into()));
        }
        Ok(Self(parts))
    }

    /// True when every photo is guaranteed a distinct name.
    pub fn has_unique_part(&self) -> bool {
        self.0.iter().any(|p| matches!(p, Part::Name | Part::Seq(_)))
    }

    /// The new file stem (no extension) for `asset`, numbered `seq`.
    pub fn render(&self, asset: &Asset, seq: usize) -> String {
        let fallback = file_date(asset.mtime);
        let date = asset.taken_at.as_deref().and_then(split_date).or_else(|| split_date(&fallback));
        let date = date.as_ref();
        let mut out = String::new();
        for p in &self.0 {
            match p {
                Part::Text(t) => out.push_str(t),
                Part::Name => out.push_str(stem(&asset.file_name)),
                Part::Seq(w) => out.push_str(&format!("{seq:0w$}")),
                Part::Date => out.push_str(&date.map_or("nodate".into(), |d| format!("{}-{}-{}", d.0, d.1, d.2))),
                Part::Time => out.push_str(&date.map_or("notime".into(), |d| d.3.clone())),
                Part::Year => out.push_str(date.map_or("nodate", |d| d.0)),
                Part::Month => out.push_str(date.map_or("nodate", |d| d.1)),
                Part::Day => out.push_str(date.map_or("nodate", |d| d.2)),
                Part::Camera => out.push_str(asset.camera_model.as_deref().unwrap_or("unknown")),
                Part::Rating => out.push_str(&asset.rating.to_string()),
            }
        }
        sanitize(&out)
    }
}

fn bad(token: &str) -> Error {
    Error::Invalid(format!("unknown template token {{{token}}}"))
}

fn stem(file_name: &str) -> &str {
    file_name.rsplit_once('.').map_or(file_name, |(s, _)| s)
}

/// Unix seconds → `"YYYY-MM-DD HH:MM:SS"` (UTC), via Howard Hinnant's
/// days-to-civil algorithm.
pub fn file_date(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}", rem / 3600, rem / 60 % 60, rem % 60)
}

/// `"2024-05-01 13:45:10"` → (`2024`, `05`, `01`, `134510`).
fn split_date(s: &str) -> Option<(&str, &str, &str, String)> {
    let (date, time) = s.trim().split_once([' ', 'T']).unwrap_or((s.trim(), ""));
    let mut d = date.split(['-', ':']);
    let (y, m, day) = (d.next()?, d.next()?, d.next()?);
    let ok = |v: &str, n| v.len() == n && v.bytes().all(|b| b.is_ascii_digit());
    if !(ok(y, 4) && ok(m, 2) && ok(day, 2)) {
        return None;
    }
    let t: String = time.chars().filter(char::is_ascii_digit).take(6).collect();
    Some((y, m, day, if t.len() == 6 { t } else { "000000".into() }))
}

/// Makes a string safe as a file name on Windows, macOS and Linux.
pub fn sanitize(s: &str) -> String {
    let cleaned: String =
        s.chars().map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c }).collect();
    let trimmed = cleaned.trim().trim_end_matches(['.', ' ']);
    let out = if trimmed.is_empty() { "photo".to_string() } else { trimmed.to_string() };
    // Reserved device names on Windows.
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1",
        "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(&out)) {
        format!("{out}_")
    } else {
        out
    }
}

// ------------------------------------------------------------ batch rename

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RenamePlan {
    pub id: i64,
    pub from: String,
    pub to: String,
    /// Why this file can't be renamed (name taken, …). `None` = OK.
    pub problem: Option<String>,
}

/// Works out new paths for `assets` (in order, numbered from `start`)
/// without touching the disk. Detects clashes with existing files and
/// within the batch; unchanged names are reported with no problem.
pub fn plan_renames(assets: &[Asset], template: &NameTemplate, start: usize) -> Vec<RenamePlan> {
    let leaving: HashSet<PathBuf> = assets.iter().map(|a| PathBuf::from(&a.path)).collect();
    let mut taken: HashSet<String> = HashSet::new();
    assets
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let from = PathBuf::from(&a.path);
            let ext = Path::new(&a.file_name).extension().map(|e| e.to_string_lossy().to_string());
            let name = match &ext {
                Some(e) => format!("{}.{e}", template.render(a, start + i)),
                None => template.render(a, start + i),
            };
            let to = from.with_file_name(&name);
            let key = to.to_string_lossy().to_lowercase();
            let problem = if !taken.insert(key) {
                Some("two photos would get the same name".to_string())
            } else if to != from && to.exists() && !leaving.contains(&to) && !same_file_case_change(&from, &to) {
                Some("a file with this name already exists".to_string())
            } else {
                None
            };
            RenamePlan { id: a.id, from: a.path.clone(), to: to.to_string_lossy().into(), problem }
        })
        .collect()
}

fn same_file_case_change(from: &Path, to: &Path) -> bool {
    from.to_string_lossy().to_lowercase() == to.to_string_lossy().to_lowercase()
}

/// Sidecars that travel with a photo: `photo.ext.xmp` and Prism's private
/// `photo.ext.prism.xmp`.
fn sidecars(p: &Path) -> [PathBuf; 2] {
    let s = p.to_string_lossy();
    [PathBuf::from(format!("{s}.xmp")), PathBuf::from(format!("{s}.prism.xmp"))]
}

/// `(asset id, new path or error message)`.
pub type IdText = (i64, String);

/// Renames the files in `plans` that have no problem (and actually change),
/// together with their sidecars. Goes through temporary names first, so
/// swaps and rotations inside one batch work. Returns `(id, new path)` for
/// each success and `(id, error)` for each failure.
pub fn execute_renames(plans: &[RenamePlan]) -> (Vec<IdText>, Vec<IdText>) {
    let mut ok = Vec::new();
    let mut failed = Vec::new();
    let todo: Vec<&RenamePlan> = plans.iter().filter(|p| p.problem.is_none() && p.from != p.to).collect();
    let tmp_for = |from: &Path, i: usize, k: usize| {
        from.with_file_name(format!(".prism-rename-{}-{i}-{k}.tmp", std::process::id()))
    };
    // Phase 1: move every file (and its sidecars) aside to temporary names.
    let mut staged = Vec::new();
    for (i, p) in todo.iter().enumerate() {
        let from = PathBuf::from(&p.from);
        let tmp = tmp_for(&from, i, 0);
        if let Err(e) = std::fs::rename(&from, &tmp) {
            failed.push((p.id, e.to_string()));
            continue;
        }
        let mut side = Vec::new();
        for (k, sc) in sidecars(&from).into_iter().enumerate() {
            let t = tmp_for(&from, i, k + 1);
            if sc.exists() && std::fs::rename(&sc, &t).is_ok() {
                side.push((k, sc, t));
            }
        }
        staged.push((*p, tmp, side));
    }
    // Phase 2: move into place; on failure put everything back.
    for (p, tmp, side) in staged {
        let (from, to) = (PathBuf::from(&p.from), PathBuf::from(&p.to));
        let res = if to.exists() && !same_file_case_change(&from, &to) {
            Err("a file with this name already exists".to_string())
        } else {
            std::fs::rename(&tmp, &to).map_err(|e| e.to_string())
        };
        match res {
            Ok(()) => {
                let targets = sidecars(&to);
                for (k, orig, t) in side {
                    let dest = if targets[k].exists() { orig } else { targets[k].clone() };
                    let _ = std::fs::rename(&t, dest);
                }
                ok.push((p.id, p.to.clone()));
            }
            Err(e) => {
                let _ = std::fs::rename(&tmp, &from);
                for (_, orig, t) in side {
                    let _ = std::fs::rename(&t, orig);
                }
                failed.push((p.id, e));
            }
        }
    }
    (ok, failed)
}

// ------------------------------------------------------------------ export

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ExportFormat {
    #[default]
    Jpeg,
    Png,
    Tiff,
    /// Lossless WebP (the pure-Rust encoder has no lossy mode).
    Webp,
}

impl ExportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Jpeg => "jpg",
            ExportFormat::Png => "png",
            ExportFormat::Tiff => "tif",
            ExportFormat::Webp => "webp",
        }
    }

    /// Encodes `img`; `quality` (10…100) only affects JPEG.
    pub fn encode(self, img: &image::RgbImage, quality: u8) -> Result<Vec<u8>> {
        use image::ImageEncoder;
        let mut out = Vec::new();
        let (w, h) = img.dimensions();
        let ty = image::ExtendedColorType::Rgb8;
        let res = match self {
            ExportFormat::Jpeg => image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality.clamp(10, 100))
                .write_image(img, w, h, ty),
            ExportFormat::Png => image::codecs::png::PngEncoder::new(&mut out).write_image(img, w, h, ty),
            ExportFormat::Tiff => {
                image::codecs::tiff::TiffEncoder::new(std::io::Cursor::new(&mut out)).write_image(img, w, h, ty)
            }
            ExportFormat::Webp => image::codecs::webp::WebPEncoder::new_lossless(&mut out).write_image(img, w, h, ty),
        };
        res.map_err(|e| Error::Invalid(format!("encode {self:?}: {e}")))?;
        Ok(out)
    }
}

/// Picks `dir/stem.ext`, adding `-1`, `-2`, … if the name is taken.
pub fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let mut target = dir.join(format!("{stem}.{ext}"));
    let mut n = 1;
    while target.exists() {
        target = dir.join(format!("{stem}-{n}.{ext}"));
        n += 1;
    }
    target
}

#[cfg(test)]
pub(crate) mod tests_support {
    use crate::Asset;

    /// A plain JPEG asset for tests in other modules.
    pub fn asset() -> Asset {
        Asset {
            id: 1,
            path: "/x/a.jpg".into(),
            folder: "/x".into(),
            file_name: "a.jpg".into(),
            ext: "jpg".into(),
            size: 1,
            mtime: 0,
            width: None,
            height: None,
            taken_at: None,
            camera_make: None,
            camera_model: None,
            lens: None,
            iso: None,
            f_number: None,
            exposure_time: None,
            focal_length: None,
            orientation: 1,
            rating: 0,
            color_label: 0,
            tagged: false,
            caption: None,
            developed: false,
            gps: None,
            has_text: false,
            needs_place: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::develop::{CropRect, LocalAdjustment};

    fn asset(id: i64, path: &Path, taken: Option<&str>) -> Asset {
        Asset {
            id,
            path: path.to_string_lossy().into(),
            folder: path.parent().unwrap().to_string_lossy().into(),
            file_name: path.file_name().unwrap().to_string_lossy().into(),
            ext: "jpg".into(),
            size: 1,
            mtime: 0,
            width: None,
            height: None,
            taken_at: taken.map(Into::into),
            camera_make: None,
            camera_model: Some("EOS 6D".into()),
            lens: None,
            iso: None,
            f_number: None,
            exposure_time: None,
            focal_length: None,
            orientation: 1,
            rating: 4,
            color_label: 0,
            tagged: false,
            caption: None,
            developed: false,
            gps: None,
            has_text: false,
            needs_place: false,
        }
    }

    #[test]
    fn preset_keeps_per_photo_work() {
        let mut base = DevelopSettings::default();
        base.geometry.crop = Some(CropRect { x: 0.1, y: 0.1, w: 0.5, h: 0.5 });
        base.lens.enabled = true;
        base.local.push(LocalAdjustment::default());
        base.light.exposure = 1.0;
        let bw = &builtin_presets().into_iter().find(|p| p.name == "Black & white").unwrap().settings;
        let out = apply_preset(&base, bw);
        assert_eq!(out.color.saturation, -100.0);
        assert_eq!(out.light.exposure, 0.0, "preset replaces the look");
        assert_eq!(out.geometry, base.geometry);
        assert!(out.lens.enabled);
        assert_eq!(out.local.len(), 1);
        assert!(look_of(&out).local.is_empty());
        assert!(builtin_presets().iter().all(|p| !p.settings.is_identity()));
    }

    #[test]
    fn templates_render_and_sanitize() {
        let a = asset(1, Path::new("/x/IMG_0001.CR2"), Some("2024-05-01 13:45:10"));
        let t = NameTemplate::parse("{date}_{name}").unwrap();
        assert_eq!(t.render(&a, 1), "2024-05-01_IMG_0001");
        let t = NameTemplate::parse("Trip {year}-{month} #{seq:3} {camera} r{rating}").unwrap();
        assert_eq!(t.render(&a, 7), "Trip 2024-05 #007 EOS 6D r4");
        assert_eq!(NameTemplate::parse("{time}").unwrap().render(&a, 1), "134510");
        let mut undated = asset(2, Path::new("/x/b.jpg"), None);
        undated.mtime = 1_709_210_096; // 2024-02-29 12:34:56 UTC
        assert_eq!(NameTemplate::parse("{date}_{time}-{seq}").unwrap().render(&undated, 2), "2024-02-29_123456-2");
        assert_eq!(file_date(0), "1970-01-01 00:00:00");
        assert_eq!(file_date(951_782_400), "2000-02-29 00:00:00");
        assert_eq!(NameTemplate::parse("a/b:c?").unwrap().render(&a, 1), "a_b_c_");
        assert_eq!(NameTemplate::parse("con").unwrap().render(&a, 1), "con_");
        assert!(NameTemplate::parse("{bogus}").is_err());
        assert!(NameTemplate::parse("{name").is_err());
        assert!(!NameTemplate::parse("{date}").unwrap().has_unique_part());
    }

    #[test]
    fn rename_plans_detect_clashes_and_swaps_execute() {
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n);
        for n in ["a.jpg", "b.jpg", "c.jpg", "taken.jpg"] {
            std::fs::write(p(n), n).unwrap();
        }
        std::fs::write(p("a.jpg.xmp"), "sidecar a").unwrap();
        std::fs::write(p("b.jpg.prism.xmp"), "sidecar b").unwrap();
        // Same name for two photos → clash; existing unrelated file → clash.
        let t = NameTemplate::parse("same").unwrap();
        let plans = plan_renames(&[asset(1, &p("a.jpg"), None), asset(2, &p("b.jpg"), None)], &t, 1);
        assert!(plans[0].problem.is_none() && plans[1].problem.is_some());
        let t = NameTemplate::parse("taken").unwrap();
        assert!(plan_renames(&[asset(3, &p("c.jpg"), None)], &t, 1)[0].problem.is_some());

        // Rotation a→b, b→c, c→a via {seq} ordering works through temp names.
        let t = NameTemplate::parse("{seq}").unwrap();
        let assets = [asset(1, &p("a.jpg"), None), asset(2, &p("b.jpg"), None)];
        let plans = plan_renames(&assets, &t, 1);
        assert!(plans.iter().all(|p| p.problem.is_none()), "{plans:?}");
        let (ok, failed) = execute_renames(&plans);
        assert_eq!((ok.len(), failed.len()), (2, 0));
        assert_eq!(std::fs::read_to_string(p("1.jpg")).unwrap(), "a.jpg");
        assert_eq!(std::fs::read_to_string(p("2.jpg")).unwrap(), "b.jpg");
        assert_eq!(std::fs::read_to_string(p("1.jpg.xmp")).unwrap(), "sidecar a");
        assert_eq!(std::fs::read_to_string(p("2.jpg.prism.xmp")).unwrap(), "sidecar b");

        // Swap: 1.jpg ↔ 2.jpg.
        let t = NameTemplate::parse("{seq}").unwrap();
        let plans = plan_renames(&[asset(2, &p("2.jpg"), None), asset(1, &p("1.jpg"), None)], &t, 1);
        assert!(plans.iter().all(|p| p.problem.is_none()), "{plans:?}");
        execute_renames(&plans);
        assert_eq!(std::fs::read_to_string(p("1.jpg")).unwrap(), "b.jpg");
        assert_eq!(std::fs::read_to_string(p("2.jpg")).unwrap(), "a.jpg");
        assert_eq!(std::fs::read_to_string(p("2.jpg.xmp")).unwrap(), "sidecar a");
        assert_eq!(std::fs::read_to_string(p("1.jpg.prism.xmp")).unwrap(), "sidecar b");
        assert!(!p("1.jpg.xmp").exists() && !p("2.jpg.prism.xmp").exists());
    }

    #[test]
    fn every_export_format_round_trips() {
        let img = image::RgbImage::from_fn(17, 9, |x, y| image::Rgb([x as u8 * 10, y as u8 * 20, 128]));
        for f in [ExportFormat::Jpeg, ExportFormat::Png, ExportFormat::Tiff, ExportFormat::Webp] {
            let bytes = f.encode(&img, 90).unwrap();
            let back = image::load_from_memory(&bytes).unwrap().to_rgb8();
            assert_eq!(back.dimensions(), (17, 9), "{f:?}");
            if f != ExportFormat::Jpeg {
                assert_eq!(back, img, "{f:?} must be lossless");
            }
        }
    }
}
