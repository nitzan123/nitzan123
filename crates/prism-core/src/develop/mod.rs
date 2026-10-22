//! Non-destructive develop settings. The original file is never modified;
//! these settings live in the catalog and in the XMP sidecar and are applied
//! at render time by [`pipeline::render`].
//!
//! Slider conventions: most controls are `-100..=100` with `0` = no change.
//! Every struct has `#[serde(default)]`, so older/partial JSON always loads.

pub mod auto;
pub(crate) mod buffer;
pub mod masks;
pub mod pipeline;
pub mod retouch;

use serde::{Deserialize, Serialize};

pub use pipeline::{render, render_to_rgb8};

pub const SETTINGS_VERSION: u32 = 1;

/// Centre hues (degrees) of the 8-band colour mixer:
/// red, orange, yellow, green, aqua, blue, purple, magenta.
pub const BAND_HUES: [f32; 8] = [0.0, 30.0, 60.0, 120.0, 180.0, 225.0, 270.0, 315.0];
pub const BAND_NAMES: [&str; 8] = ["Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct DevelopSettings {
    pub version: u32,
    pub white_balance: WhiteBalance,
    pub light: Light,
    pub color: Color,
    pub mixer: ColorMixer,
    pub curve: ToneCurve,
    pub split_tone: SplitTone,
    pub detail: Detail,
    pub vignette: Vignette,
    pub geometry: Geometry,
    pub local: Vec<LocalAdjustment>,
    pub lens: crate::lens::LensSettings,
    /// Heal/clone spots, in source image coordinates.
    pub spots: Vec<retouch::Spot>,
}

impl Default for DevelopSettings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            white_balance: Default::default(),
            light: Default::default(),
            color: Default::default(),
            mixer: Default::default(),
            curve: Default::default(),
            split_tone: Default::default(),
            detail: Default::default(),
            vignette: Default::default(),
            geometry: Default::default(),
            local: Vec::new(),
            lens: Default::default(),
            spots: Vec::new(),
        }
    }
}

impl DevelopSettings {
    /// True when rendering with these settings changes nothing.
    pub fn is_identity(&self) -> bool {
        let mut s = self.clone();
        s.version = SETTINGS_VERSION;
        s.local.retain(|l| l.enabled);
        s.spots.retain(|p| p.enabled);
        s == Self::default()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct WhiteBalance {
    /// -100 (cooler/bluer) … 100 (warmer/yellower).
    pub temperature: f32,
    /// -100 (greener) … 100 (more magenta).
    pub tint: f32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Light {
    /// Stops, -5 … 5.
    pub exposure: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    /// Local (mid-frequency) contrast.
    pub clarity: f32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Color {
    /// Saturation boost weighted towards muted colours.
    pub vibrance: f32,
    pub saturation: f32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ColorMixer {
    /// Per band, -100 … 100 → ±30° hue rotation.
    pub hue: [f32; 8],
    pub saturation: [f32; 8],
    pub luminance: [f32; 8],
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ToneCurve {
    /// Control points in display space, `[x, y]` in 0…1, sorted by x.
    pub points: Vec<[f32; 2]>,
}

impl Default for ToneCurve {
    fn default() -> Self {
        Self { points: vec![[0.0, 0.0], [1.0, 1.0]] }
    }
}

impl ToneCurve {
    pub fn is_identity(&self) -> bool {
        self.points.iter().all(|[x, y]| (x - y).abs() < 1e-4)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct SplitTone {
    /// Degrees 0…360.
    pub highlight_hue: f32,
    /// 0 … 100.
    pub highlight_saturation: f32,
    pub shadow_hue: f32,
    pub shadow_saturation: f32,
    /// -100 (favour shadows) … 100 (favour highlights).
    pub balance: f32,
}

impl Default for SplitTone {
    fn default() -> Self {
        Self { highlight_hue: 45.0, highlight_saturation: 0.0, shadow_hue: 220.0, shadow_saturation: 0.0, balance: 0.0 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Detail {
    /// 0 … 150.
    pub sharpen_amount: f32,
    /// Pixels at preview scale, 0.5 … 3.
    pub sharpen_radius: f32,
    /// 0 … 100; ignores differences below this many 1/255 steps ×0.1.
    pub sharpen_threshold: f32,
    /// Luminance noise reduction, 0 … 100.
    pub noise_reduction: f32,
}

impl Default for Detail {
    fn default() -> Self {
        Self { sharpen_amount: 0.0, sharpen_radius: 1.0, sharpen_threshold: 0.0, noise_reduction: 0.0 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Vignette {
    /// -100 (darken edges) … 100 (lighten edges).
    pub amount: f32,
    /// Where the falloff starts, 0 (centre) … 100 (corners).
    pub midpoint: f32,
    /// 0 (hard) … 100 (very soft).
    pub feather: f32,
}

impl Default for Vignette {
    fn default() -> Self {
        Self { amount: 0.0, midpoint: 50.0, feather: 50.0 }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Geometry {
    /// Quarter turns clockwise, 0…3.
    pub rotate90: u8,
    pub flip_horizontal: bool,
    /// Degrees, -45 … 45. The image is scaled so no empty corners appear.
    pub straighten: f32,
    /// Normalised crop of the straightened frame.
    pub crop: Option<CropRect>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CropRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl CropRect {
    pub fn clamped(self) -> Self {
        let x = self.x.clamp(0.0, 0.99);
        let y = self.y.clamp(0.0, 0.99);
        Self { x, y, w: self.w.clamp(0.01, (1.0 - x).max(0.01)), h: self.h.clamp(0.01, (1.0 - y).max(0.01)) }
    }
}

/// A masked adjustment ("local edit"). The mask decides *where*; the
/// fields decide *what*.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct LocalAdjustment {
    pub enabled: bool,
    pub name: String,
    pub mask: masks::Mask,
    pub invert: bool,
    /// Optional luminance targeting: only affect pixels whose display
    /// luminance is inside `[low, high]` (0…1), with soft edges.
    pub luma_range: Option<[f32; 2]>,
    pub exposure: f32,
    pub contrast: f32,
    pub saturation: f32,
    pub temperature: f32,
    pub tint: f32,
    /// Gaussian blur strength 0…100 (e.g. background blur).
    pub blur: f32,
}

impl Default for LocalAdjustment {
    fn default() -> Self {
        Self {
            enabled: true,
            name: String::new(),
            mask: masks::Mask::default(),
            invert: false,
            luma_range: None,
            exposure: 0.0,
            contrast: 0.0,
            saturation: 0.0,
            temperature: 0.0,
            tint: 0.0,
            blur: 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_identity_and_partial_json_loads() {
        assert!(DevelopSettings::default().is_identity());
        let s: DevelopSettings = serde_json::from_str(r#"{"light":{"exposure":1.0}}"#).unwrap();
        assert_eq!(s.light.exposure, 1.0);
        assert_eq!(s.vignette.midpoint, 50.0);
        assert!(!s.is_identity());
    }

    #[test]
    fn disabled_local_only_is_identity() {
        let mut s = DevelopSettings::default();
        s.local.push(LocalAdjustment { enabled: false, exposure: 1.0, ..Default::default() });
        assert!(s.is_identity());
    }

    #[test]
    fn json_roundtrip() {
        let mut s = DevelopSettings::default();
        s.mixer.hue[3] = 20.0;
        s.geometry.crop = Some(CropRect { x: 0.1, y: 0.1, w: 0.5, h: 0.5 });
        s.local.push(LocalAdjustment { mask: masks::Mask::Sky { sensitivity: 50.0 }, ..Default::default() });
        let j = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<DevelopSettings>(&j).unwrap(), s);
    }
}
