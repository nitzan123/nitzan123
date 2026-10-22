//! Text on photos ("subject, place, date"), drawn as part of a photo's
//! develop settings so it shows in previews, thumbnails and exports.
//!
//! The three parts are joined with ", " on one line and wrapped at the
//! commas when the line would be too wide, e.g.
//! `Vicuña, Centro Cultural Parwa Museo, Chinchero, 01.10.2026`.
//! Bidirectional text (e.g. Hebrew) is reordered for display.

use crate::develop::buffer::blur_plane;
use ab_glyph::{point, Font, FontRef, PxScale, ScaleFont};
use image::RgbImage;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

static REGULAR: &[u8] = include_bytes!("../assets/fonts/DejaVuSans.ttf");
static BOLD: &[u8] = include_bytes!("../assets/fonts/DejaVuSans-Bold.ttf");

fn font(bold: bool) -> &'static FontRef<'static> {
    static R: OnceLock<FontRef<'static>> = OnceLock::new();
    static B: OnceLock<FontRef<'static>> = OnceLock::new();
    let (cell, bytes) = if bold { (&B, BOLD) } else { (&R, REGULAR) };
    cell.get_or_init(|| FontRef::try_from_slice(bytes).expect("bundled font is valid"))
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Position {
    TopLeft,
    TopRight,
    #[default]
    BottomLeft,
    BottomCenter,
    BottomRight,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TextColor {
    /// Black on light backgrounds, white on dark ones.
    #[default]
    Auto,
    White,
    Black,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DateFormat {
    /// `17.05.2024`
    #[default]
    Dotted,
    /// `17/05/2024`
    Numeric,
    /// `17 May 2024`
    LongDate,
    /// `17 May 2024, 14:03`
    Long,
    /// `2024-05-17 14:03`
    Iso,
}

/// The text on one photo. Empty parts are left out.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct TextOverlay {
    /// What or where exactly, typed by the user (e.g. `Vicuña`).
    pub subject: String,
    /// Place, usually the town from GPS (e.g. `Chinchero`).
    pub place: String,
    /// Already formatted (e.g. `01.10.2026`).
    pub date: String,
    pub position: Position,
    /// Text height as % of the image's short side.
    pub size: f32,
    pub color: TextColor,
    pub italic: bool,
    pub bold: bool,
}

impl Default for TextOverlay {
    fn default() -> Self {
        Self {
            subject: String::new(),
            place: String::new(),
            date: String::new(),
            position: Position::BottomLeft,
            size: 2.5,
            color: TextColor::Auto,
            italic: true,
            bold: false,
        }
    }
}

impl TextOverlay {
    /// The parts that will be shown, in order.
    pub fn parts(&self) -> Vec<&str> {
        [&self.subject, &self.place, &self.date].into_iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.parts().is_empty()
    }

    /// Draws the text onto `img` (no-op when empty).
    pub fn draw(&self, img: &mut RgbImage) {
        let parts = self.parts();
        if parts.is_empty() || img.width() < 16 || img.height() < 16 {
            return;
        }
        let short = img.width().min(img.height()) as f32;
        let px = (short * self.size.clamp(1.0, 15.0) / 100.0).max(9.0);
        let margin = (short * 0.02).round().max(2.0);
        let max_w = img.width() as f32 - margin * 2.0;
        let lines = wrap(&parts, |s| text_width(s, px, self.bold, self.italic), max_w);
        draw_lines(img, &lines, px, margin, self);
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
        DateFormat::Dotted => format!("{day:02}.{m:02}.{y}"),
        DateFormat::Numeric => format!("{day:02}/{m:02}/{y}"),
        DateFormat::LongDate => format!("{day} {} {y}", MONTHS[m - 1]),
        DateFormat::Long => match hm {
            Some(t) => format!("{day} {} {y}, {t}", MONTHS[m - 1]),
            None => format!("{day} {} {y}", MONTHS[m - 1]),
        },
        DateFormat::Iso => match hm {
            Some(t) => format!("{y}-{m:02}-{day:02} {t}"),
            None => format!("{y}-{m:02}-{day:02}"),
        },
    })
}

/// Joins `parts` with ", " into as few lines as fit `max_w`, breaking at
/// the commas first and at spaces inside a part only when it alone is too
/// wide. Every line but the last keeps its trailing comma.
pub fn wrap(parts: &[&str], width: impl Fn(&str) -> f32, max_w: f32) -> Vec<String> {
    // Tokens: each part's words; a part's last word carries the comma.
    let mut tokens: Vec<(String, bool)> = Vec::new(); // (word, ends a part)
    for (i, part) in parts.iter().enumerate() {
        let words: Vec<&str> = part.split_whitespace().collect();
        for (j, w) in words.iter().enumerate() {
            let last = j + 1 == words.len();
            let comma = last && i + 1 < parts.len();
            tokens.push((if comma { format!("{w},") } else { (*w).to_string() }, last));
        }
    }
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut i = 0;
    while i < tokens.len() {
        // Prefer adding a whole part (up to its end) to the current line.
        let end = (i..tokens.len()).find(|&k| tokens[k].1).unwrap_or(tokens.len() - 1);
        let chunk = tokens[i..=end].iter().map(|t| t.0.as_str()).collect::<Vec<_>>().join(" ");
        let joined = if cur.is_empty() { chunk.clone() } else { format!("{cur} {chunk}") };
        if width(&joined) <= max_w || cur.is_empty() && width(&chunk) <= max_w {
            cur = joined;
            i = end + 1;
            continue;
        }
        if !cur.is_empty() {
            lines.push(std::mem::take(&mut cur));
            continue;
        }
        // A single part wider than the line: fall back to word wrapping.
        let w = &tokens[i].0;
        let joined = if cur.is_empty() { w.clone() } else { format!("{cur} {w}") };
        if width(&joined) <= max_w || cur.is_empty() {
            cur = joined;
        } else {
            lines.push(std::mem::replace(&mut cur, w.clone()));
        }
        i += 1;
        if tokens[i - 1].1 {
            continue;
        }
        // Finish this long part word by word.
        while i < tokens.len() {
            let w = &tokens[i].0;
            let joined = format!("{cur} {w}");
            if width(&joined) <= max_w {
                cur = joined;
            } else {
                lines.push(std::mem::replace(&mut cur, w.clone()));
            }
            i += 1;
            if tokens[i - 1].1 {
                break;
            }
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
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

/// Horizontal shear for synthetic italics (≈12°).
const SLANT: f32 = 0.21;

fn layout(line: &str, px: f32, bold: bool) -> (Vec<ab_glyph::Glyph>, f32) {
    let f = font(bold).as_scaled(PxScale::from(px));
    let mut x = 0f32;
    let mut prev = None;
    let glyphs = visual(line)
        .chars()
        .map(|ch| {
            let id = f.glyph_id(ch);
            if let Some(p) = prev {
                x += f.kern(p, id);
            }
            let g = id.with_scale_and_position(px, point(x, 0.0));
            x += f.h_advance(id);
            prev = Some(id);
            g
        })
        .collect();
    (glyphs, x)
}

fn text_width(line: &str, px: f32, bold: bool, italic: bool) -> f32 {
    layout(line, px, bold).1 + if italic { px * SLANT } else { 0.0 }
}

fn draw_lines(img: &mut RgbImage, lines: &[String], px: f32, margin: f32, t: &TextOverlay) {
    let f = font(t.bold).as_scaled(PxScale::from(px));
    let line_h = (f.height() + f.line_gap()).ceil();
    let glow_r = (px * 0.12).max(1.0);
    let pad = (glow_r * 3.0).ceil() as i64;
    let slant = if t.italic { SLANT } else { 0.0 };

    let laid: Vec<(Vec<ab_glyph::Glyph>, f32)> = lines.iter().map(|l| layout(l, px, t.bold)).collect();
    let extra = (px * slant).ceil();
    let block_w = (laid.iter().map(|(_, w)| *w).fold(0f32, f32::max) + extra).ceil() as i64;
    let block_h = (line_h * lines.len() as f32).ceil() as i64;
    let (mw, mh) = ((block_w + pad * 2) as usize, (block_h + pad * 2) as usize);

    // Rasterise into a coverage mask, aligned per the position.
    let mut mask = vec![0f32; mw * mh];
    for (i, (glyphs, w)) in laid.iter().enumerate() {
        let x0 = pad as f32
            + match t.position {
                Position::TopRight | Position::BottomRight => block_w as f32 - w - extra,
                Position::BottomCenter => (block_w as f32 - w - extra) / 2.0,
                _ => 0.0,
            };
        let baseline = pad as f32 + i as f32 * line_h + f.ascent();
        for g in glyphs {
            let mut g = g.clone();
            g.position = point(g.position.x + x0, baseline);
            if let Some(og) = font(t.bold).outline_glyph(g) {
                let b = og.px_bounds();
                og.draw(|gx, gy, cov| {
                    let y = b.min.y as i64 + gy as i64;
                    let shear = ((baseline - y as f32) * slant).round() as i64;
                    let x = b.min.x as i64 + gx as i64 + shear;
                    if (0..mw as i64).contains(&x) && (0..mh as i64).contains(&y) {
                        let m = &mut mask[y as usize * mw + x as usize];
                        *m = (*m + cov).min(1.0);
                    }
                });
            }
        }
    }

    let (iw, ih) = (img.width() as i64, img.height() as i64);
    let ox = match t.position {
        Position::TopRight | Position::BottomRight => iw - margin as i64 - block_w - pad,
        Position::BottomCenter => (iw - block_w) / 2 - pad,
        _ => margin as i64 - pad,
    };
    let oy = match t.position {
        Position::TopLeft | Position::TopRight => margin as i64 - pad,
        _ => ih - margin as i64 - block_h - pad,
    };

    // Auto colour: dark text on a light background and vice versa.
    let dark_text = match t.color {
        TextColor::Black => true,
        TextColor::White => false,
        TextColor::Auto => {
            let (mut sum, mut n) = (0f32, 0f32);
            for y in (oy.max(0)..(oy + mh as i64).min(ih)).step_by(2) {
                for x in (ox.max(0)..(ox + mw as i64).min(iw)).step_by(2) {
                    let p = img.get_pixel(x as u32, y as u32).0;
                    sum += 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32;
                    n += 1.0;
                }
            }
            n > 0.0 && sum / n > 125.0
        }
    };
    let (ink, halo) = if dark_text { (20.0, 255.0) } else { (255.0, 0.0) };
    // A soft halo in the opposite colour keeps the text readable on busy
    // backgrounds; lighter for dark text so it stays a "printed" look.
    let halo_strength = if dark_text { 0.45 } else { 0.65 };
    let glow = blur_plane(&mask, mw, mh, glow_r);
    for my in 0..mh as i64 {
        for mx in 0..mw as i64 {
            let (x, y) = (ox + mx, oy + my);
            if !(0..iw).contains(&x) || !(0..ih).contains(&y) {
                continue;
            }
            let i = my as usize * mw + mx as usize;
            let g = (glow[i] * 1.6).min(1.0) * halo_strength;
            let tx = mask[i];
            if g <= 0.0 && tx <= 0.0 {
                continue;
            }
            let p = img.get_pixel_mut(x as u32, y as u32);
            for ch in p.0.iter_mut() {
                let v = *ch as f32 + (halo - *ch as f32) * g;
                *ch = (v + (ink - v) * tx).round().clamp(0.0, 255.0) as u8;
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
        assert_eq!(format_date(t, DateFormat::Dotted).unwrap(), "17.05.2024");
        assert_eq!(format_date(t, DateFormat::Numeric).unwrap(), "17/05/2024");
        assert_eq!(format_date(t, DateFormat::LongDate).unwrap(), "17 May 2024");
        assert_eq!(format_date(t, DateFormat::Long).unwrap(), "17 May 2024, 14:03");
        assert_eq!(format_date(t, DateFormat::Iso).unwrap(), "2024-05-17 14:03");
        assert_eq!(format_date("2024-05-17", DateFormat::Long).unwrap(), "17 May 2024");
        assert!(format_date("garbage", DateFormat::Dotted).is_none());
        assert!(format_date("2024-13-01", DateFormat::Dotted).is_none());
    }

    #[test]
    fn wraps_at_commas_like_a_handwritten_caption() {
        let w = |s: &str| s.chars().count() as f32;
        // Fits: one line.
        assert_eq!(
            wrap(&["Parque Isaac Rabin", "Lima", "23.09.2026"], w, 60.0),
            vec!["Parque Isaac Rabin, Lima, 23.09.2026"]
        );
        // Too wide: break after a comma, keeping parts whole.
        assert_eq!(
            wrap(&["Parque Isaac Rabin", "Lima", "23.09.2026"], w, 24.0),
            vec!["Parque Isaac Rabin,", "Lima, 23.09.2026"]
        );
        // A single part wider than the line falls back to words.
        assert_eq!(
            wrap(&["Centro Cultural Parwa Museo", "Chinchero"], w, 16.0),
            vec!["Centro Cultural", "Parwa Museo,", "Chinchero"]
        );
        assert!(wrap(&[], w, 10.0).is_empty());
    }

    #[test]
    fn hebrew_is_reordered_for_display() {
        assert_eq!(visual("שלום"), "םולש");
        assert_eq!(visual("Tel Aviv"), "Tel Aviv");
    }

    fn bbox(img: &RgbImage, ink: impl Fn(u8) -> bool) -> Option<(u32, u32, u32, u32)> {
        let mut bb: Option<(u32, u32, u32, u32)> = None;
        for (x, y, p) in img.enumerate_pixels() {
            if ink(p.0[0]) {
                bb = Some(match bb {
                    None => (x, y, x, y),
                    Some((a, b, c, d)) => (a.min(x), b.min(y), c.max(x), d.max(y)),
                });
            }
        }
        bb
    }

    fn sample() -> TextOverlay {
        TextOverlay {
            subject: "Vicuña".into(),
            place: "Chinchero".into(),
            date: "01.10.2026".into(),
            ..Default::default()
        }
    }

    #[test]
    fn auto_colour_positions_and_empty() {
        // Light background → dark text, bottom left.
        let mut light = RgbImage::from_pixel(900, 600, image::Rgb([230, 230, 230]));
        sample().draw(&mut light);
        let (x0, y0, x1, y1) = bbox(&light, |v| v < 60).expect("dark text on light photo");
        assert!(x0 < 60 && y1 > 540 && x1 < 600 && y0 > 480, "{x0},{y0},{x1},{y1}");

        // Dark background → white text, bottom centre.
        let mut dark = RgbImage::from_pixel(900, 600, image::Rgb([30, 30, 30]));
        TextOverlay { position: Position::BottomCenter, ..sample() }.draw(&mut dark);
        let (x0, _, x1, _) = bbox(&dark, |v| v > 200).expect("light text on dark photo");
        assert!((x0 as i64 + x1 as i64 - 900).abs() < 30, "centred: {x0}..{x1}");

        // Nothing to say → untouched.
        let mut img = RgbImage::from_pixel(400, 300, image::Rgb([120, 120, 120]));
        let before = img.clone();
        TextOverlay::default().draw(&mut img);
        assert_eq!(img, before);
        assert!(TextOverlay { subject: "  ".into(), ..Default::default() }.is_empty());
    }

    #[test]
    fn narrow_photos_wrap_onto_two_lines() {
        let t = TextOverlay {
            subject: "Parque Isaac Rabin".into(),
            place: "Lima".into(),
            date: "23.09.2026".into(),
            size: 7.0,
            ..Default::default()
        };
        let mut portrait = RgbImage::from_pixel(400, 700, image::Rgb([230, 230, 230]));
        t.draw(&mut portrait);
        let (x0, y0, x1, y1) = bbox(&portrait, |v| v < 60).unwrap();
        assert!(x1 < 400 - 4 && x0 > 2, "stays inside: {x0}..{x1}");
        assert!((y1 - y0) as f32 > 400.0 * 0.07 * 1.6, "two lines tall: {}", y1 - y0);
    }
}
