//! EXIF extraction. Works for JPEG, TIFF and the TIFF-based RAW formats
//! (CR2, NEF, ARW, DNG, ORF, PEF, RW2 …). Missing tags are simply `None`.

use exif::{In, Reader, Tag, Value};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufReader, Read};
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
    let Some(exif) = parse(path) else {
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
        gps: gps(&exif),
    }
}

/// Tolerant parsing: files from some phones/editors carry a malformed IFD
/// link, so every field that did parse is kept instead of losing them all.
fn tolerant(r: std::result::Result<exif::Exif, exif::Error>) -> Option<exif::Exif> {
    match r {
        Ok(e) => Some(e),
        Err(exif::Error::PartialResult(partial)) => Some(partial.into_inner().0),
        Err(_) => None,
    }
}

fn parse(path: &Path) -> Option<exif::Exif> {
    let file = File::open(path).ok()?;
    if let Some(e) = tolerant(Reader::new().continue_on_error(true).read_from_container(&mut BufReader::new(file))) {
        return Some(e);
    }
    // HEIF whose EXIF item the generic parser refuses (e.g. Xiaomi's ~190 KB
    // blocks): let the HEIF decoder locate it and parse the TIFF directly.
    if crate::imaging::is_heif_ext(&crate::imaging::ext_of(path)) {
        let data = std::fs::read(path).ok()?;
        let tiff = heic::DecoderConfig::new().extract_exif(&data).ok()??;
        return tolerant(Reader::new().continue_on_error(true).read_raw(tiff.into_owned()));
    }
    // RAW containers the parser doesn't recognise: Olympus ORF ("IIRO"),
    // Panasonic RW2 ("IIU\0"), Fuji RAF, Canon CR3 (ISO-BMFF with a TIFF
    // inside its CMT box). Look for a TIFF header near the start, then for an
    // embedded JPEG carrying EXIF.
    if !crate::raw::is_raw_ext(&crate::imaging::ext_of(path)) {
        return None;
    }
    let mut head = Vec::new();
    File::open(path).ok()?.take(8 << 20).read_to_end(&mut head).ok()?;
    if head.len() > 8 && &head[..2] == b"II" && matches!(&head[2..4], b"RO" | b"RS" | b"U\0") {
        let mut tiff = head.clone();
        tiff[2..4].copy_from_slice(&[0x2A, 0x00]);
        if let Some(e) = tolerant(Reader::new().continue_on_error(true).read_raw(tiff)) {
            return Some(e);
        }
    }
    let limit = head.len().min(1 << 20);
    for i in 0..limit.saturating_sub(8) {
        let w = &head[i..i + 4];
        if w == b"II*\0" || w == b"MM\0*" {
            if let Some(e) = tolerant(Reader::new().continue_on_error(true).read_raw(head[i..].to_vec())) {
                if e.get_field(Tag::Model, In::PRIMARY).is_some()
                    || e.get_field(Tag::DateTimeOriginal, In::PRIMARY).is_some()
                {
                    return Some(e);
                }
            }
        }
    }
    let jpeg = crate::raw::extract_embedded_jpeg(&head)?;
    tolerant(Reader::new().continue_on_error(true).read_from_container(&mut std::io::Cursor::new(jpeg)))
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

/// Capture date encoded in a file name by phones and messaging apps, e.g.
/// `WhatsApp Image 2026-10-20 at 21.32.51.jpeg`, `IMG-20261020-WA0001.jpg`,
/// `PXL_20261020_213251123.jpg`, `Screenshot_2026-10-20-21-32-51.png`.
/// Returns `YYYY-MM-DD HH:MM:SS` (time `00:00:00` when absent).
pub fn date_from_filename(name: &str) -> Option<String> {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(
            r"(?:^|[^0-9])((?:19|20)\d{2})[-_.]?(0[1-9]|1[0-2])[-_.]?(0[1-9]|[12]\d|3[01])(?:[ _T-]*(?:at[ _]?)?([01]\d|2[0-3])[.:_-]?([0-5]\d)(?:[.:_-]?([0-5]\d))?\d{0,3})?(?:[^0-9]|$)",
        )
        .expect("valid regex")
    });
    let c = re.captures(name)?;
    let g = |i: usize| c.get(i).map(|m| m.as_str());
    let (y, mo, d) = (g(1)?, g(2)?, g(3)?);
    let day_ok =
        d.parse::<u32>().ok()? <= [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31][mo.parse::<usize>().ok()? - 1];
    day_ok.then(|| format!("{y}-{mo}-{d} {}:{}:{}", g(4).unwrap_or("00"), g(5).unwrap_or("00"), g(6).unwrap_or("00")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_from_phone_and_whatsapp_file_names() {
        let d = |n| date_from_filename(n);
        assert_eq!(d("WhatsApp Image 2026-10-20 at 21.32.51.jpeg").as_deref(), Some("2026-10-20 21:32:51"));
        assert_eq!(d("WhatsApp Image 2026-10-20 at 21.32.51 (1).jpeg").as_deref(), Some("2026-10-20 21:32:51"));
        assert_eq!(d("IMG-20261020-WA0001.jpg").as_deref(), Some("2026-10-20 00:00:00"));
        assert_eq!(d("PXL_20261020_213251123.jpg").as_deref(), Some("2026-10-20 21:32:51"));
        assert_eq!(d("IMG_20240501_134510.jpg").as_deref(), Some("2024-05-01 13:45:10"));
        assert_eq!(d("Screenshot_2026-10-20-21-32-51.png").as_deref(), Some("2026-10-20 21:32:51"));
        assert_eq!(d("20240501.jpg").as_deref(), Some("2024-05-01 00:00:00"));
        for none in
            ["12.png", "IMG_1234.JPG", "DSC01234.ARW", "photo_123456789.jpg", "2024-02-30.jpg", "x2024050199.jpg"]
        {
            assert_eq!(d(none), None, "{none}");
        }
    }

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
    fn reads_exif_from_orf_style_tiff_variant() {
        // An Olympus ORF is a little-endian TIFF whose magic is "IIRO"
        // instead of "II*\0". One IFD entry: Model (0x0110), ASCII "E-M1\0".
        let mut orf = b"IIRO".to_vec();
        orf.extend_from_slice(&8u32.to_le_bytes());
        orf.extend_from_slice(&1u16.to_le_bytes());
        orf.extend_from_slice(&0x0110u16.to_le_bytes());
        orf.extend_from_slice(&2u16.to_le_bytes()); // ASCII
        orf.extend_from_slice(&5u32.to_le_bytes());
        orf.extend_from_slice(&26u32.to_le_bytes()); // value offset
        orf.extend_from_slice(&0u32.to_le_bytes()); // next IFD
        orf.extend_from_slice(b"E-M1\0");
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.ORF");
        std::fs::write(&p, &orf).unwrap();
        assert_eq!(read_exif(&p).camera_model.as_deref(), Some("E-M1"));
        // The same bytes under a non-RAW extension are not second-guessed.
        let q = dir.path().join("x.jpg");
        std::fs::write(&q, &orf).unwrap();
        assert!(read_exif(&q).camera_model.is_none());
    }

    #[test]
    fn missing_file_gives_defaults() {
        let info = read_exif(Path::new("/definitely/not/here.jpg"));
        assert_eq!(info.orientation, 1);
        assert!(info.taken_at.is_none());
    }
}
