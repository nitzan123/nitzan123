//! Metadata embedded in exported files: EXIF (camera, exposure, capture
//! date, GPS, copyright) and an XMP packet (rating, keywords, caption,
//! copyright, plus capture date and GPS for readers that only use XMP).
//!
//! Files are encoded first and the metadata is then spliced into the
//! container: JPEG APP1 segments, PNG `eXIf`/`iTXt` chunks, WebP `VP8X` +
//! `EXIF`/`XMP ` chunks. TIFF output is written with the metadata as tags.

use crate::error::{Error, Result};
use crate::workflow::ExportFormat;
use crate::Asset;

/// What goes into an exported file. Build with [`ExportMeta::from_asset`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExportMeta {
    pub make: Option<String>,
    pub model: Option<String>,
    pub lens: Option<String>,
    pub iso: Option<u32>,
    pub f_number: Option<f32>,
    pub exposure_time: Option<f32>,
    pub focal_length: Option<f32>,
    /// `YYYY-MM-DD HH:MM:SS`.
    pub taken_at: Option<String>,
    /// `[latitude, longitude]`.
    pub gps: Option<[f64; 2]>,
    pub copyright: Option<String>,
    pub rating: u8,
    pub caption: Option<String>,
    pub keywords: Vec<String>,
}

impl ExportMeta {
    /// Metadata of `asset` for export. `include_gps = false` strips the
    /// location (for sharing publicly).
    pub fn from_asset(a: &Asset, keywords: Vec<String>, include_gps: bool, copyright: Option<String>) -> Self {
        Self {
            make: a.camera_make.clone(),
            model: a.camera_model.clone(),
            lens: a.lens.clone(),
            iso: a.iso,
            f_number: a.f_number,
            exposure_time: a.exposure_time,
            focal_length: a.focal_length,
            taken_at: a.taken_at.clone(),
            gps: a.gps.filter(|_| include_gps),
            copyright: copyright.map(|c| c.trim().to_string()).filter(|c| !c.is_empty()),
            rating: a.rating,
            caption: a.caption.clone().filter(|c| !c.trim().is_empty()),
            keywords,
        }
    }
}

// --------------------------------------------------------------------- EXIF

const SOFTWARE: &str = concat!("Prism Studio ", env!("CARGO_PKG_VERSION"));

enum Val {
    Ascii(String),
    Short(u16),
    Long(u32),
    Rationals(Vec<(u32, u32)>),
    Bytes(Vec<u8>),
    Undefined(Vec<u8>),
}

impl Val {
    fn encode(&self) -> (u16, u32, Vec<u8>) {
        match self {
            Val::Ascii(s) => {
                let mut b: Vec<u8> = s.bytes().filter(|c| *c != 0).collect();
                b.push(0);
                (2, b.len() as u32, b)
            }
            Val::Short(v) => (3, 1, v.to_le_bytes().to_vec()),
            Val::Long(v) => (4, 1, v.to_le_bytes().to_vec()),
            Val::Rationals(r) => {
                (5, r.len() as u32, r.iter().flat_map(|(n, d)| [n.to_le_bytes(), d.to_le_bytes()].concat()).collect())
            }
            Val::Bytes(b) => (1, b.len() as u32, b.clone()),
            Val::Undefined(b) => (7, b.len() as u32, b.clone()),
        }
    }
}

/// Serialises one IFD located at `offset` (from the TIFF header), with its
/// out-of-line values right after it.
fn ifd(mut entries: Vec<(u16, Val)>, offset: u32) -> Vec<u8> {
    entries.sort_by_key(|e| e.0);
    let n = entries.len() as u32;
    let mut data_off = offset + 2 + 12 * n + 4;
    let (mut head, mut data) = (Vec::new(), Vec::new());
    head.extend((n as u16).to_le_bytes());
    for (tag, v) in &entries {
        let (ty, count, bytes) = v.encode();
        head.extend(tag.to_le_bytes());
        head.extend(ty.to_le_bytes());
        head.extend(count.to_le_bytes());
        if bytes.len() <= 4 {
            let mut inline = bytes.clone();
            inline.resize(4, 0);
            head.extend(inline);
        } else {
            head.extend(data_off.to_le_bytes());
            data_off += bytes.len() as u32 + (bytes.len() as u32 & 1);
            data.extend(&bytes);
            if bytes.len() % 2 == 1 {
                data.push(0);
            }
        }
    }
    head.extend(0u32.to_le_bytes()); // no next IFD
    head.extend(data);
    head
}

fn rational(v: f32, denom: u32) -> (u32, u32) {
    ((v.max(0.0) * denom as f32).round() as u32, denom)
}

fn exif_date(s: &str) -> Option<String> {
    let s = s.trim();
    let ok = s.len() >= 19 && s.as_bytes()[4] == b'-' && s.as_bytes()[7] == b'-';
    ok.then(|| format!("{}:{}:{} {}", &s[0..4], &s[5..7], &s[8..10], &s[11..19]))
}

fn dms(v: f64) -> Vec<(u32, u32)> {
    let v = v.abs();
    let d = v.trunc();
    let m = ((v - d) * 60.0).trunc();
    let s = ((v - d) * 60.0 - m) * 60.0;
    vec![(d as u32, 1), (m as u32, 1), ((s * 10_000.0).round() as u32, 10_000)]
}

/// A complete little-endian TIFF/EXIF block (what follows `Exif\0\0`).
pub fn exif_block(m: &ExportMeta) -> Vec<u8> {
    let mut ifd0 = vec![(0x0112, Val::Short(1)), (0x0131, Val::Ascii(SOFTWARE.into()))];
    let text = |tag: u16, v: &Option<String>, out: &mut Vec<(u16, Val)>| {
        if let Some(s) = v.as_ref().filter(|s| !s.trim().is_empty()) {
            out.push((tag, Val::Ascii(s.trim().into())));
        }
    };
    text(0x010F, &m.make, &mut ifd0);
    text(0x0110, &m.model, &mut ifd0);
    text(0x8298, &m.copyright, &mut ifd0);
    let date = m.taken_at.as_deref().and_then(exif_date);
    if let Some(d) = &date {
        ifd0.push((0x0132, Val::Ascii(d.clone())));
    }

    let mut exif = vec![(0x9000, Val::Undefined(b"0232".to_vec()))];
    if let Some(d) = &date {
        exif.push((0x9003, Val::Ascii(d.clone())));
        exif.push((0x9004, Val::Ascii(d.clone())));
    }
    if let Some(t) = m.exposure_time.filter(|t| *t > 0.0) {
        let r = if t < 1.0 { (1, (1.0 / t).round() as u32) } else { rational(t, 10) };
        exif.push((0x829A, Val::Rationals(vec![r])));
    }
    if let Some(f) = m.f_number.filter(|f| *f > 0.0) {
        exif.push((0x829D, Val::Rationals(vec![rational(f, 10)])));
    }
    if let Some(iso) = m.iso {
        exif.push((0x8827, Val::Short(iso.min(65_535) as u16)));
    }
    if let Some(f) = m.focal_length.filter(|f| *f > 0.0) {
        exif.push((0x920A, Val::Rationals(vec![rational(f, 10)])));
    }
    text(0xA434, &m.lens, &mut exif);

    let gps = m.gps.map(|[lat, lon]| {
        vec![
            (0x0000, Val::Bytes(vec![2, 3, 0, 0])),
            (0x0001, Val::Ascii(if lat >= 0.0 { "N" } else { "S" }.into())),
            (0x0002, Val::Rationals(dms(lat))),
            (0x0003, Val::Ascii(if lon >= 0.0 { "E" } else { "W" }.into())),
            (0x0004, Val::Rationals(dms(lon))),
        ]
    });

    // IFD0 holds pointers to the sub-IFDs; its size doesn't depend on their
    // values, so lay it out once with placeholders to learn the offsets.
    let with_ptrs = |exif_at: u32, gps_at: Option<u32>, base: &[(u16, Val)]| {
        let mut e: Vec<(u16, Val)> = base
            .iter()
            .map(|(t, v)| {
                let v = match v {
                    Val::Ascii(s) => Val::Ascii(s.clone()),
                    Val::Short(x) => Val::Short(*x),
                    _ => unreachable!("IFD0 only holds text and shorts"),
                };
                (*t, v)
            })
            .collect();
        e.push((0x8769, Val::Long(exif_at)));
        if let Some(g) = gps_at {
            e.push((0x8825, Val::Long(g)));
        }
        e
    };
    let ifd0_len = ifd(with_ptrs(0, gps.as_ref().map(|_| 0), &ifd0), 8).len() as u32;
    let exif_at = 8 + ifd0_len;
    let exif_bytes = ifd(exif, exif_at);
    let gps_at = gps.as_ref().map(|_| exif_at + exif_bytes.len() as u32);

    let mut out = b"II*\0".to_vec();
    out.extend(8u32.to_le_bytes());
    out.extend(ifd(with_ptrs(exif_at, gps_at, &ifd0), 8));
    out.extend(exif_bytes);
    if let (Some(g), Some(at)) = (gps, gps_at) {
        out.extend(ifd(g, at));
    }
    out
}

// ---------------------------------------------------------------------- XMP

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// XMP GPS coordinate: `"31,46.6020N"`.
fn xmp_coord(v: f64, pos: char, neg: char) -> String {
    let a = v.abs();
    format!("{},{:.4}{}", a.trunc(), (a - a.trunc()) * 60.0, if v >= 0.0 { pos } else { neg })
}

pub fn xmp_packet(m: &ExportMeta) -> String {
    let mut attrs = format!("\n   xmp:Rating=\"{}\"\n   xmp:CreatorTool=\"{SOFTWARE}\"", m.rating.min(5));
    if let Some(d) = m.taken_at.as_deref().and_then(exif_date) {
        let iso = format!("{}-{}-{}T{}", &d[0..4], &d[5..7], &d[8..10], &d[11..]);
        attrs += &format!("\n   exif:DateTimeOriginal=\"{iso}\"\n   photoshop:DateCreated=\"{iso}\"");
    }
    if let Some([lat, lon]) = m.gps {
        attrs += &format!(
            "\n   exif:GPSLatitude=\"{}\"\n   exif:GPSLongitude=\"{}\"",
            xmp_coord(lat, 'N', 'S'),
            xmp_coord(lon, 'E', 'W')
        );
    }
    let alt = |name: &str, v: &str| {
        format!(
            "   <{name}>\n    <rdf:Alt>\n     <rdf:li xml:lang=\"x-default\">{}</rdf:li>\n    </rdf:Alt>\n   </{name}>\n",
            esc(v)
        )
    };
    let mut body = String::new();
    if !m.keywords.is_empty() {
        body += "   <dc:subject>\n    <rdf:Bag>\n";
        for k in &m.keywords {
            body += &format!("     <rdf:li>{}</rdf:li>\n", esc(k));
        }
        body += "    </rdf:Bag>\n   </dc:subject>\n";
    }
    if let Some(c) = &m.caption {
        body += &alt("dc:description", c);
    }
    if let Some(c) = &m.copyright {
        body += &alt("dc:rights", c);
    }
    format!(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n\
<x:xmpmeta xmlns:x=\"adobe:ns:meta/\" x:xmptk=\"Prism Studio\">\n\
 <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n\
  <rdf:Description rdf:about=\"\"\n   xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\"\n   xmlns:dc=\"http://purl.org/dc/elements/1.1/\"\n   xmlns:exif=\"http://ns.adobe.com/exif/1.0/\"\n   xmlns:photoshop=\"http://ns.adobe.com/photoshop/1.0/\"{attrs}>\n{body}  </rdf:Description>\n </rdf:RDF>\n</x:xmpmeta>\n<?xpacket end=\"w\"?>"
    )
}

// ---------------------------------------------------------------- containers

const XMP_NS_JPEG: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";

fn jpeg_segment(marker: u8, payload: &[u8]) -> Result<Vec<u8>> {
    let len = payload.len() + 2;
    if len > 0xFFFF {
        return Err(Error::Invalid("metadata too large for a JPEG segment".into()));
    }
    let mut s = vec![0xFF, marker];
    s.extend((len as u16).to_be_bytes());
    s.extend(payload);
    Ok(s)
}

fn embed_jpeg(bytes: &[u8], exif: &[u8], xmp: &str) -> Result<Vec<u8>> {
    if !bytes.starts_with(&[0xFF, 0xD8]) {
        return Err(Error::Invalid("not a JPEG".into()));
    }
    // After SOI and a JFIF APP0 if present.
    let mut at = 2;
    if bytes.get(2..4) == Some(&[0xFF, 0xE0]) {
        at = 4 + u16::from_be_bytes([bytes[4], bytes[5]]) as usize;
    }
    let mut out = bytes[..at].to_vec();
    out.extend(jpeg_segment(0xE1, &[b"Exif\0\0".as_slice(), exif].concat())?);
    out.extend(jpeg_segment(0xE1, &[XMP_NS_JPEG, xmp.as_bytes()].concat())?);
    out.extend(&bytes[at..]);
    Ok(out)
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn png_chunk(ty: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut c = (data.len() as u32).to_be_bytes().to_vec();
    c.extend(ty);
    c.extend(data);
    c.extend(crc32(&[ty.as_slice(), data].concat()).to_be_bytes());
    c
}

fn embed_png(bytes: &[u8], exif: &[u8], xmp: &str) -> Result<Vec<u8>> {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") || bytes.get(12..16) != Some(b"IHDR") {
        return Err(Error::Invalid("not a PNG".into()));
    }
    let ihdr_end = 8 + 12 + u32::from_be_bytes(bytes[8..12].try_into().unwrap()) as usize;
    let mut itxt = b"XML:com.adobe.xmp\0\0\0\0\0".to_vec(); // keyword, uncompressed, no lang/translation
    itxt.extend(xmp.as_bytes());
    let mut out = bytes[..ihdr_end].to_vec();
    out.extend(png_chunk(b"eXIf", exif));
    out.extend(png_chunk(b"iTXt", &itxt));
    out.extend(&bytes[ihdr_end..]);
    Ok(out)
}

fn riff_chunk(ty: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut c = ty.to_vec();
    c.extend((data.len() as u32).to_le_bytes());
    c.extend(data);
    if data.len() % 2 == 1 {
        c.push(0);
    }
    c
}

fn embed_webp(bytes: &[u8], exif: &[u8], xmp: &str, w: u32, h: u32) -> Result<Vec<u8>> {
    if bytes.get(0..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WEBP") {
        return Err(Error::Invalid("not a WebP".into()));
    }
    let body = &bytes[12..];
    if body.starts_with(b"VP8X") {
        return Err(Error::Invalid("extended WebP input is not supported".into()));
    }
    let mut vp8x = vec![0x08 | 0x04, 0, 0, 0]; // EXIF + XMP flags
    vp8x.extend(&(w - 1).to_le_bytes()[..3]);
    vp8x.extend(&(h - 1).to_le_bytes()[..3]);
    let mut payload = b"WEBP".to_vec();
    payload.extend(riff_chunk(b"VP8X", &vp8x));
    payload.extend(body);
    payload.extend(riff_chunk(b"EXIF", exif));
    payload.extend(riff_chunk(b"XMP ", xmp.as_bytes()));
    let mut out = b"RIFF".to_vec();
    out.extend((payload.len() as u32).to_le_bytes());
    out.extend(payload);
    Ok(out)
}

/// TIFF ASCII fields must be 7-bit; the exact text is kept in the XMP.
fn ascii(s: &str) -> String {
    s.replace('©', "(c)").chars().map(|c| if c.is_ascii() && c != '\0' { c } else { '?' }).collect()
}

fn tiff_with_meta(img: &image::RgbImage, m: &ExportMeta, xmp: &str) -> Result<Vec<u8>> {
    use tiff::encoder::{colortype::RGB8, TiffEncoder};
    use tiff::tags::Tag;
    let err = |e: tiff::TiffError| Error::Invalid(format!("encode TIFF: {e}"));
    let mut out = std::io::Cursor::new(Vec::new());
    {
        let mut enc = TiffEncoder::new(&mut out).map_err(err)?;
        let mut im = enc.new_image::<RGB8>(img.width(), img.height()).map_err(err)?;
        let dir = im.encoder();
        dir.write_tag(Tag::Software, ascii(SOFTWARE).as_str()).map_err(err)?;
        for (tag, v) in [(Tag::Make, &m.make), (Tag::Model, &m.model), (Tag::Copyright, &m.copyright)] {
            if let Some(v) = v.as_deref().filter(|v| !v.trim().is_empty()) {
                dir.write_tag(tag, ascii(v.trim()).as_str()).map_err(err)?;
            }
        }
        if let Some(d) = m.taken_at.as_deref().and_then(exif_date) {
            dir.write_tag(Tag::DateTime, d.as_str()).map_err(err)?;
        }
        dir.write_tag(Tag::Unknown(700), xmp.as_bytes()).map_err(err)?; // XMP
        im.write_data(img.as_raw()).map_err(err)?;
    }
    Ok(out.into_inner())
}

/// Encodes `img` as `format` with `meta` embedded.
pub fn encode_with_meta(
    format: ExportFormat,
    img: &image::RgbImage,
    quality: u8,
    meta: &ExportMeta,
) -> Result<Vec<u8>> {
    let xmp = xmp_packet(meta);
    if format == ExportFormat::Tiff {
        return tiff_with_meta(img, meta, &xmp);
    }
    let bytes = format.encode(img, quality)?;
    let exif = exif_block(meta);
    match format {
        ExportFormat::Jpeg => embed_jpeg(&bytes, &exif, &xmp),
        ExportFormat::Png => embed_png(&bytes, &exif, &xmp),
        ExportFormat::Webp => embed_webp(&bytes, &exif, &xmp, img.width(), img.height()),
        ExportFormat::Tiff => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use exif::{In, Tag, Value};

    fn meta() -> ExportMeta {
        ExportMeta {
            make: Some("Canon".into()),
            model: Some("EOS 6D".into()),
            lens: Some("EF85mm f/1.8 USM".into()),
            iso: Some(400),
            f_number: Some(1.8),
            exposure_time: Some(1.0 / 250.0),
            focal_length: Some(85.0),
            taken_at: Some("2024-05-01 13:45:10".into()),
            gps: Some([31.7767, -35.2345]),
            copyright: Some("© Test & Co".into()),
            rating: 4,
            caption: Some("Old <city>".into()),
            keywords: vec!["Jerusalem".into(), "trip".into()],
        }
    }

    fn read(bytes: &[u8]) -> exif::Exif {
        exif::Reader::new().read_from_container(&mut std::io::Cursor::new(bytes)).expect("EXIF readable")
    }

    fn check_exif(e: &exif::Exif, full: bool) {
        let s = |t| e.get_field(t, In::PRIMARY).map(|f| f.display_value().to_string().replace('"', ""));
        assert_eq!(s(Tag::Make).as_deref(), Some("Canon"));
        assert_eq!(s(Tag::Model).as_deref(), Some("EOS 6D"));
        assert_eq!(s(Tag::DateTime).as_deref(), Some("2024-05-01 13:45:10"));
        assert!(s(Tag::Copyright).unwrap().contains("Test & Co"));
        if !full {
            return;
        }
        assert_eq!(s(Tag::DateTimeOriginal).as_deref(), Some("2024-05-01 13:45:10"));
        assert_eq!(s(Tag::ExposureTime).as_deref(), Some("1/250"));
        assert_eq!(s(Tag::FNumber).as_deref(), Some("1.8"));
        assert_eq!(s(Tag::PhotographicSensitivity).as_deref(), Some("400"));
        assert_eq!(s(Tag::LensModel).as_deref(), Some("EF85mm f/1.8 USM"));
        let deg = |t| match &e.get_field(t, In::PRIMARY).unwrap().value {
            Value::Rational(r) => r[0].to_f64() + r[1].to_f64() / 60.0 + r[2].to_f64() / 3600.0,
            v => panic!("{v:?}"),
        };
        assert!((deg(Tag::GPSLatitude) - 31.7767).abs() < 1e-5);
        assert!((deg(Tag::GPSLongitude) - 35.2345).abs() < 1e-5);
        assert_eq!(s(Tag::GPSLongitudeRef).as_deref(), Some("W"));
        // Orientation is 1: pixels are already upright after export.
        assert_eq!(e.get_field(Tag::Orientation, In::PRIMARY).unwrap().value.get_uint(0), Some(1));
    }

    #[test]
    fn every_format_carries_exif_and_xmp_and_still_decodes() {
        let img = image::RgbImage::from_fn(33, 21, |x, y| image::Rgb([x as u8 * 7, y as u8 * 11, 90]));
        for f in [ExportFormat::Jpeg, ExportFormat::Png, ExportFormat::Webp, ExportFormat::Tiff] {
            let bytes = encode_with_meta(f, &img, 90, &meta()).unwrap();
            let back = image::load_from_memory(&bytes).unwrap_or_else(|e| panic!("{f:?}: {e}")).to_rgb8();
            assert_eq!(back.dimensions(), (33, 21), "{f:?}");
            if f != ExportFormat::Jpeg {
                assert_eq!(back, img, "{f:?} pixels must be untouched");
            }
            check_exif(&read(&bytes), f != ExportFormat::Tiff);
            let text = String::from_utf8_lossy(&bytes);
            assert!(text.contains("<rdf:li>Jerusalem</rdf:li>"), "{f:?} XMP keywords");
            assert!(text.contains("Old &lt;city&gt;"), "{f:?} XMP caption escaped");
            assert!(text.contains("exif:GPSLongitude=\"35,14.0700W\""), "{f:?} XMP GPS");
            assert!(text.contains("xmp:Rating=\"4\""));
        }
    }

    #[test]
    fn location_can_be_stripped_and_empty_fields_are_omitted() {
        let mut a = crate::workflow::tests_support::asset();
        a.gps = Some([1.0, 2.0]);
        let m = ExportMeta::from_asset(&a, vec![], false, Some("  ".into()));
        assert_eq!((m.gps, m.copyright.clone()), (None, None));
        let bytes = encode_with_meta(ExportFormat::Jpeg, &image::RgbImage::new(4, 4), 80, &m).unwrap();
        let e = read(&bytes);
        assert!(e.get_field(Tag::GPSLatitude, In::PRIMARY).is_none());
        assert!(e.get_field(Tag::Copyright, In::PRIMARY).is_none());
        assert!(!String::from_utf8_lossy(&bytes).contains("GPSLatitude"));
    }
}
