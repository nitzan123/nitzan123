//! EXIF extraction. Works for JPEG, TIFF and the TIFF-based RAW formats
//! (CR2, NEF, ARW, DNG, ORF, PEF, RW2 …). Missing tags are simply `None`.

use exif::{In, Reader, Tag, Value};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ExifInfo {
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// `YYYY-MM-DD HH:MM:SS`, taken from DateTimeOriginal (fallback DateTime).
    pub taken_at: Option<String>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens: Option<String>,
    pub iso: Option<u32>,
    pub f_number: Option<f32>,
    /// Seconds.
    pub exposure_time: Option<f32>,
    pub focal_length: Option<f32>,
    /// EXIF orientation 1..=8.
    pub orientation: u16,
    /// `[latitude, longitude]` in decimal degrees (south/west negative).
    pub gps: Option<[f64; 2]>,
}

pub fn read_exif(path: &Path) -> ExifInfo {
    let Ok(file) = File::open(path) else {
        return ExifInfo { orientation: 1, ..Default::default() };
    };
    // Tolerant parsing: files from some phones/editors carry a malformed
    // IFD link; keep every field that did parse instead of losing them all.
    let exif = match Reader::new().continue_on_error(true).read_from_container(&mut BufReader::new(file)) {
        Ok(e) => e,
        Err(exif::Error::PartialResult(partial)) => partial.into_inner().0,
        Err(_) => return ExifInfo { orientation: 1, ..Default::default() },
    };

    let text = |tag: Tag| -> Option<String> {
        let f = exif.get_field(tag, In::PRIMARY)?;
        match &f.value {
            Value::Ascii(v) => v
                .first()
                .map(|b| String::from_utf8_lossy(b).trim().trim_end_matches('\0').to_string())
                .filter(|s| !s.is_empty()),
            _ => Some(f.display_value().to_string()),
        }
    };
    let uint = |tag: Tag| -> Option<u32> { exif.get_field(tag, In::PRIMARY)?.value.get_uint(0) };
    let rational = |tag: Tag| -> Option<f32> {
        match &exif.get_field(tag, In::PRIMARY)?.value {
            Value::Rational(v) => v.first().map(|r| r.to_f64() as f32),
            _ => None,
        }
    };

    let taken_at = text(Tag::DateTimeOriginal).or_else(|| text(Tag::DateTime)).map(|s| normalize_exif_date(&s));

    ExifInfo {
        width: uint(Tag::PixelXDimension).or_else(|| uint(Tag::ImageWidth)),
        height: uint(Tag::PixelYDimension).or_else(|| uint(Tag::ImageLength)),
        taken_at,
        camera_make: text(Tag::Make),
        camera_model: text(Tag::Model),
        lens: text(Tag::LensModel),
        iso: uint(Tag::PhotographicSensitivity),
        f_number: rational(Tag::FNumber),
        exposure_time: rational(Tag::ExposureTime),
        focal_length: rational(Tag::FocalLength),
        orientation: uint(Tag::Orientation).map(|o| o as u16).filter(|o| (1..=8).contains(o)).unwrap_or(1),
        gps: gps(&exif),
    }
}

fn gps(exif: &exif::Exif) -> Option<[f64; 2]> {
    let coord = |value: Tag, reference: Tag| -> Option<f64> {
        let Value::Rational(v) = &exif.get_field(value, In::PRIMARY)?.value else { return None };
        let parts: Vec<f64> = v.iter().map(|r| r.to_f64()).collect();
        let r = exif.get_field(reference, In::PRIMARY)?.display_value().to_string();
        dms_to_degrees(&parts, &r)
    };
    let lat = coord(Tag::GPSLatitude, Tag::GPSLatitudeRef)?;
    let lon = coord(Tag::GPSLongitude, Tag::GPSLongitudeRef)?;
    // 0,0 is what many devices write when they have no fix.
    let valid = lat.abs() <= 90.0 && lon.abs() <= 180.0 && (lat, lon) != (0.0, 0.0);
    valid.then_some([lat, lon])
}

/// Degrees/minutes/seconds + hemisphere reference (`N`/`S`/`E`/`W`) to
/// signed decimal degrees.
pub fn dms_to_degrees(dms: &[f64], reference: &str) -> Option<f64> {
    let d = *dms.first()?;
    let m = dms.get(1).copied().unwrap_or(0.0);
    let s = dms.get(2).copied().unwrap_or(0.0);
    if !(d.is_finite() && m.is_finite() && s.is_finite()) {
        return None;
    }
    let v = d + m / 60.0 + s / 3600.0;
    let negative = matches!(reference.trim().trim_matches('"').chars().next(), Some('S' | 's' | 'W' | 'w'));
    Some(if negative { -v } else { v })
}

/// EXIF dates look like `2024:05:17 14:03:22`; store them sortable as
/// `2024-05-17 14:03:22`.
pub fn normalize_exif_date(raw: &str) -> String {
    let raw = raw.trim().trim_matches('"');
    let mut out = raw.to_string();
    if raw.len() >= 10 && raw.as_bytes()[4] == b':' && raw.as_bytes()[7] == b':' {
        out.replace_range(4..5, "-");
        out.replace_range(7..8, "-");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_exif_dates() {
        assert_eq!(normalize_exif_date("2024:05:17 14:03:22"), "2024-05-17 14:03:22");
        assert_eq!(normalize_exif_date("2024-05-17"), "2024-05-17");
    }

    #[test]
    fn converts_dms_with_hemisphere() {
        let tel_aviv = dms_to_degrees(&[32.0, 5.0, 7.08], "N").unwrap();
        assert!((tel_aviv - 32.0853).abs() < 1e-4);
        assert!(dms_to_degrees(&[33.0, 52.0, 0.0], "S").unwrap() < 0.0);
        assert!(dms_to_degrees(&[151.0, 12.0, 0.0], "W").unwrap() < 0.0);
        assert!(dms_to_degrees(&[], "N").is_none());
    }

    /// Written by piexif, whose IFD0 "next IFD" link kamadak-exif rejects in
    /// strict mode; every field must still be read.
    #[test]
    fn reads_gps_date_camera_from_imperfect_exif() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gps_piexif.jpg");
        let e = read_exif(&p);
        assert_eq!(e.taken_at.as_deref(), Some("2023-12-24 09:15:00"));
        assert_eq!(e.camera_model.as_deref(), Some("iPhone 15"));
        let [lat, lon] = e.gps.expect("gps");
        assert!((lat - 31.7767).abs() < 1e-4 && (lon + 35.2345).abs() < 1e-4, "{lat},{lon}");
    }

    #[test]
    fn missing_file_gives_defaults() {
        let info = read_exif(Path::new("/definitely/not/here.jpg"));
        assert_eq!(info.orientation, 1);
        assert!(info.taken_at.is_none());
    }
}
