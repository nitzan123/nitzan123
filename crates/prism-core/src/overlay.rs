//! Text captions burned into exported images ("date stamp"): capture date,
//! place name and custom text in a corner, white with a soft shadow.
//! Bidirectional text (e.g. Hebrew) is reordered for display.

use crate::develop::buffer::blur_plane;
use ab_glyph::{point, Font, FontRef, PxScale, ScaleFont};
use image::RgbImage;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

static FONT_BYTES: &[u8] = include_bytes!("../assets/fonts/DejaVuSans-Bold.ttf");

fn font() -> &'static FontRef<'static> {
    static FONT: OnceLock<FontRef<'static>> = OnceLock::new();
    FONT.get_or_init(|| FontRef::try_from_slice(FONT_BYTES).expect("bundled font is valid"))
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    #[default]
    BottomRight,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DateFormat {
    /// `17 May 2024, 14:03`
    #[default]
    Long,
    /// `17 May 2024`
    LongDate,
    /// `2024-05-17 14:03`
    Iso,
    /// `17/05/2024`
    Numeric,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Caption {
    pub date: bool,
    pub date_format: DateFormat,
    pub location: bool,
    pub custom_text: String,
    pub corner: Corner,
    /// Text height as % of the image's short side.
    pub size: f32,
    /// 0 … 100.
    pub opacity: f32,
    /// Language for place names (`en`, `he`, …); empty = local names.
    pub place_language: String,
}

impl Default for Caption {
    fn default() -> Self {
        Self {
            date: true,
            date_format: DateFormat::Long,
            location: true,
            custom_text: String::new(),
            corner: Corner::BottomRight,
            size: 3.0,
            opacity: 95.0,
            place_language: "en".into(),
        }
    }
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// Formats a catalog date (`YYYY-MM-DD[ HH:MM[:SS]]`). `None` if unparseable.
pub fn format_date(taken_at: &str, f: DateFormat) -> Option<String> {
    let (date, time) = taken_at.trim().split_once(' ').unwrap_or((taken_at.trim(), ""));
    let mut d = date.split(['-', ':']);
    let (y, m, day) = (d.next()?, d.next()?.parse::<usize>().ok()?, d.next()?.parse::<u32>().ok()?);
    if y.len() != 4 || !(1..=12).contains(&m) || !(1..=31).contains(&day) {
        return None;
    }
    let hm = time.get(..5).filter(|t| t.as_bytes().get(2) == Some(&b':'));
    Some(match f {
        DateFormat::Long => match hm {
            Some(t) => format!("{day} {} {y}, {t}", MONTHS[m - 1]),
            None => format!("{day} {} {y}", MONTHS[m - 1]),
        },
        DateFormat::LongDate => format!("{day} {} {y}", MONTHS[m - 1]),
        DateFormat::Iso => match hm {
            Some(t) => format!("{y}-{m:02}-{day:02} {t}"),
            None => format!("{y}-{m:02}-{day:02}"),
        },
        DateFormat::Numeric => format!("{day:02}/{m:02}/{y}"),
    })
}

/// The caption's lines, top to bottom, skipping fields that are off/empty.
pub fn caption_lines(c: &Caption, taken_at: Option<&str>, place: Option<&str>) -> Vec<String> {
    let mut lines = Vec::new();
    if c.date {
        if let Some(d) = taken_at.and_then(|t| format_date(t, c.date_format)) {
            lines.push(d);
        }
    }
    if c.location {
        if let Some(p) = place.map(str::trim).filter(|p| !p.is_empty()) {
            lines.push(p.to_string());
        }
    }
    lines.extend(c.custom_text.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from));
    lines
}

/// Reorders a logical string into visual order (RTL runs reversed).
fn visual(line: &str) -> String {
    let info = unicode_bidi::BidiInfo::new(line, None);
    match info.paragraphs.first() {
        Some(para) => info.reorder_line(para, para.range.clone()).into_owned(),
        None => line.to_string(),
    }
}

/// Draws `lines` into the chosen corner of `img`.
pub fn draw_caption(img: &mut RgbImage, lines: &[String], c: &Caption) {
    if lines.is_empty() || img.width() < 16 || img.height() < 16 {
        return;
    }
    let short = img.width().min(img.height()) as f32;
    let px = (short * c.size.clamp(1.0, 15.0) / 100.0).max(10.0);
    let font = font().as_scaled(PxScale::from(px));
    let line_h = (font.height() + font.line_gap()).ceil();
    let margin = (short * 0.03).round();
    let shadow_r = (px * 0.12).max(1.0);
    let pad = (shadow_r * 3.0).ceil() as i64;

    // Lay out every line in visual order.
    let laid: Vec<(Vec<ab_glyph::Glyph>, f32)> = lines
        .iter()
        .map(|l| {
            let mut x = 0f32;
            let mut prev = None;
            let glyphs = visual(l)
                .chars()
                .map(|ch| {
                    let id = font.glyph_id(ch);
                    if let Some(p) = prev {
                        x += font.kern(p, id);
                    }
                    let g = id.with_scale_and_position(px, point(x, 0.0));
                    x += font.h_advance(id);
                    prev = Some(id);
                    g
                })
                .collect();
            (glyphs, x)
        })
        .collect();

    let block_w = laid.iter().map(|(_, w)| *w).fold(0f32, f32::max).ceil() as i64;
    let block_h = (line_h * lines.len() as f32).ceil() as i64;
    let (mw, mh) = ((block_w + pad * 2) as usize, (block_h + pad * 2) as usize);
    let right = matches!(c.corner, Corner::TopRight | Corner::BottomRight);
    let bottom = matches!(c.corner, Corner::BottomLeft | Corner::BottomRight);

    // Rasterise into a coverage mask.
    let mut mask = vec![0f32; mw * mh];
    for (i, (glyphs, w)) in laid.iter().enumerate() {
        let x0 = pad as f32 + if right { block_w as f32 - w } else { 0.0 };
        let baseline = pad as f32 + i as f32 * line_h + font.ascent();
        for g in glyphs {
            let mut g = g.clone();
            g.position = point(g.position.x + x0, baseline);
            if let Some(og) = font.outline_glyph(g) {
                let b = og.px_bounds();
                og.draw(|gx, gy, cov| {
                    let (x, y) = (b.min.x as i64 + gx as i64, b.min.y as i64 + gy as i64);
                    if (0..mw as i64).contains(&x) && (0..mh as i64).contains(&y) {
                        let m = &mut mask[y as usize * mw + x as usize];
                        *m = (*m + cov).min(1.0);
                    }
                });
            }
        }
    }
    let shadow = blur_plane(&mask, mw, mh, shadow_r);

    // Place the block and composite: shadow (offset) then white text.
    let ox = if right { img.width() as i64 - margin as i64 - block_w - pad } else { margin as i64 - pad };
    let oy = if bottom { img.height() as i64 - margin as i64 - block_h - pad } else { margin as i64 - pad };
    let off = (px * 0.05).round().max(1.0) as i64;
    let alpha = c.opacity.clamp(0.0, 100.0) / 100.0;
    let (iw, ih) = (img.width() as i64, img.height() as i64);
    for my in 0..mh as i64 {
        for mx in 0..mw as i64 {
            let (x, y) = (ox + mx, oy + my);
            if !(0..iw).contains(&x) || !(0..ih).contains(&y) {
                continue;
            }
            let sx = mx - off;
            let sy = my - off;
            let sh = if (0..mw as i64).contains(&sx) && (0..mh as i64).contains(&sy) {
                (shadow[sy as usize * mw + sx as usize] * 1.6).min(1.0) * 0.65 * alpha
            } else {
                0.0
            };
            let tx = mask[my as usize * mw + mx as usize] * alpha;
            if sh <= 0.0 && tx <= 0.0 {
                continue;
            }
            let p = img.get_pixel_mut(x as u32, y as u32);
            for ch in p.0.iter_mut() {
                let v = *ch as f32 * (1.0 - sh);
                *ch = (v + (255.0 - v) * tx).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_formats() {
        let t = "2024-05-17 14:03:22";
        assert_eq!(format_date(t, DateFormat::Long).unwrap(), "17 May 2024, 14:03");
        assert_eq!(format_date(t, DateFormat::LongDate).unwrap(), "17 May 2024");
        assert_eq!(format_date(t, DateFormat::Iso).unwrap(), "2024-05-17 14:03");
        assert_eq!(format_date(t, DateFormat::Numeric).unwrap(), "17/05/2024");
        assert_eq!(format_date("2024-05-17", DateFormat::Long).unwrap(), "17 May 2024");
        assert!(format_date("garbage", DateFormat::Long).is_none());
        assert!(format_date("2024-13-01", DateFormat::Long).is_none());
    }

    #[test]
    fn lines_skip_missing_fields() {
        let c = Caption { custom_text: "© Nitzan\n\n".into(), ..Default::default() };
        let l = caption_lines(&c, Some("2024-05-17 14:03:22"), Some("Tel-Aviv, Israel"));
        assert_eq!(l, vec!["17 May 2024, 14:03", "Tel-Aviv, Israel", "© Nitzan"]);
        let off = Caption { date: false, location: false, ..Default::default() };
        assert!(caption_lines(&off, Some("2024-05-17"), Some("x")).is_empty());
        assert_eq!(caption_lines(&Caption::default(), None, None), Vec::<String>::new());
    }

    #[test]
    fn hebrew_is_reordered_for_display() {
        assert_eq!(visual("שלום"), "םולש");
        assert_eq!(visual("Tel Aviv"), "Tel Aviv");
    }

    fn bright_bbox(img: &RgbImage) -> Option<(u32, u32, u32, u32)> {
        let mut bb: Option<(u32, u32, u32, u32)> = None;
        for (x, y, p) in img.enumerate_pixels() {
            if p.0[0] > 200 {
                bb = Some(match bb {
                    None => (x, y, x, y),
                    Some((a, b, c, d)) => (a.min(x), b.min(y), c.max(x), d.max(y)),
                });
            }
        }
        bb
    }

    #[test]
    fn draws_in_requested_corner() {
        let lines = vec!["17 May 2024, 14:03".to_string(), "Tel-Aviv, Israel".to_string()];
        for corner in [Corner::BottomRight, Corner::TopLeft] {
            let mut img = RgbImage::from_pixel(800, 600, image::Rgb([60, 60, 60]));
            draw_caption(&mut img, &lines, &Caption { corner, ..Default::default() });
            let (x0, y0, x1, y1) = bright_bbox(&img).expect("text drawn");
            match corner {
                Corner::BottomRight => assert!(x1 > 700 && y1 > 500 && x0 > 300, "{x0},{y0},{x1},{y1}"),
                _ => assert!(x0 < 100 && y0 < 100 && x1 < 500, "{x0},{y0},{x1},{y1}"),
            }
            // Two lines → block taller than one line height (3% of 600 = 18 px).
            assert!(y1 - y0 > 24, "two lines expected, height {}", y1 - y0);
        }
    }

    #[test]
    fn shadow_darkens_background_and_nothing_drawn_without_lines() {
        let mut img = RgbImage::from_pixel(400, 300, image::Rgb([200, 200, 200]));
        let before = img.clone();
        draw_caption(&mut img, &[], &Caption::default());
        assert_eq!(img, before);
        draw_caption(&mut img, &["Hello".to_string()], &Caption::default());
        assert!(img.pixels().any(|p| p.0[0] < 150), "soft shadow visible on light background");
    }
}
