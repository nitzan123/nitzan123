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
}

pub fn read_exif(path: &Path) -> ExifInfo {
    let Ok(file) = File::open(path) else {
        return ExifInfo { orientation: 1, ..Default::default() };
    };
    let Ok(exif) = Reader::new().read_from_container(&mut BufReader::new(file)) else {
        return ExifInfo { orientation: 1, ..Default::default() };
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
    }
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
    fn missing_file_gives_defaults() {
        let info = read_exif(Path::new("/definitely/not/here.jpg"));
        assert_eq!(info.orientation, 1);
        assert!(info.taken_at.is_none());
    }
}
