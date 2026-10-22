//! Estimating where a photo without GPS was taken, from other evidence:
//!
//! - **Nearby**: a photo from the same camera with GPS, a few minutes apart.
//! - **Phone**: a photo from another device with GPS (usually a phone) taken
//!   at about the same time. Camera clocks are often on another time zone
//!   (e.g. still on home time), so the clock offset per camera is detected
//!   by finding the shift that lines up the most photos.
//! - **Timeline**: points from an imported location history (Google Maps
//!   Timeline), with the same offset detection.
//!
//! Every estimate says where it came from, how far apart in time the
//! evidence was, and whether it is "sure enough"; the UI flags the rest
//! for the user to check.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Where a photo's place (or date) came from.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Source {
    /// Nothing found.
    #[default]
    None,
    /// The photo's own metadata (GPS / camera date): exact.
    Photo,
    /// Typed or confirmed by the user.
    Typed,
    /// Another photo from the same camera, minutes apart.
    Nearby,
    /// A photo from another device (phone) taken at about the same time.
    Phone,
    /// Imported location history.
    Timeline,
    /// Date read from the file name (e.g. WhatsApp: when it was sent).
    FileName,
    /// Date from the file's modified time.
    FileDate,
}

impl Source {
    /// Needs a human look: anything inferred rather than read or typed.
    pub fn is_estimate(self) -> bool {
        matches!(self, Source::Nearby | Source::Phone | Source::Timeline | Source::FileName | Source::FileDate)
    }
}

/// A known position at a known (naive, camera-style) time.
#[derive(Debug, Clone, PartialEq)]
pub struct RefPoint {
    /// Seconds of a naive local date-time (see [`naive_secs`]).
    pub t: i64,
    pub lat: f64,
    pub lon: f64,
    /// Camera model for photo references; `None` for timeline points.
    pub camera: Option<String>,
    pub asset_id: Option<i64>,
}

/// A photo that needs a position.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub id: i64,
    pub t: i64,
    pub camera: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Estimate {
    pub id: i64,
    pub lat: f64,
    pub lon: f64,
    pub source: Source,
    /// Minutes between the photo and the evidence.
    pub gap_minutes: f64,
    /// Close enough in time and space to trust without checking.
    pub confident: bool,
    /// Human explanation, e.g. "from a phone photo 4 min apart (camera clock −8 h)".
    pub note: String,
}

/// `YYYY-MM-DD HH:MM[:SS]` → seconds since 1970-01-01 00:00 of that naive
/// date-time (no time zone is implied or applied).
pub fn naive_secs(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, time) = s.split_once([' ', 'T']).unwrap_or((s, "00:00:00"));
    let mut d = date.split(['-', ':']);
    let (y, m, day): (i64, i64, i64) = (d.next()?.parse().ok()?, d.next()?.parse().ok()?, d.next()?.parse().ok()?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&day) {
        return None;
    }
    let mut t = time.split(':').map(|p| p.get(..2).unwrap_or(p).parse::<i64>().ok());
    let (hh, mm, ss) =
        (t.next().flatten().unwrap_or(0), t.next().flatten().unwrap_or(0), t.next().flatten().unwrap_or(0));
    // Howard Hinnant's days_from_civil.
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hh * 3600 + mm * 60 + ss)
}

/// Great-circle distance in km.
pub fn distance_km(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (la1, lo1, la2, lo2) = (a.0.to_radians(), a.1.to_radians(), b.0.to_radians(), b.1.to_radians());
    let h = ((la2 - la1) / 2.0).sin().powi(2) + la1.cos() * la2.cos() * ((lo2 - lo1) / 2.0).sin().powi(2);
    6371.0 * 2.0 * h.sqrt().asin()
}

/// Index into sorted `times` of the closest value to `t`.
fn nearest(times: &[i64], t: i64) -> Option<usize> {
    if times.is_empty() {
        return None;
    }
    let i = times.partition_point(|&x| x < t);
    let cands = [i.checked_sub(1), (i < times.len()).then_some(i)];
    cands.into_iter().flatten().min_by_key(|&j| (times[j] - t).abs())
}

/// Clock offset (seconds, added to the camera's times) that lines up the
/// most `targets` with a reference within `window` seconds. Tries ±14 h in
/// half-hour steps (time-zone differences); small clock drift is absorbed
/// by `window`. Returns `(offset, matched)`; `None` when nothing lines
/// up or the best shift isn't clearly better than the alternatives.
pub fn best_offset(targets: &[i64], refs: &[i64], window: i64) -> Option<(i64, usize)> {
    if targets.is_empty() || refs.is_empty() {
        return None;
    }
    let count = |off: i64| {
        targets
            .iter()
            .filter(|&&t| nearest(refs, t + off).is_some_and(|j| (refs[j] - (t + off)).abs() <= window))
            .count()
    };
    let mut scores: Vec<(i64, usize)> = (-28..=28).map(|k| k * 1800).map(|off| (off, count(off))).collect();
    // Most matches; on ties, the smaller shift.
    scores.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.abs().cmp(&b.0.abs())));
    let (off, best) = scores[0];
    if best == 0 {
        return None;
    }
    // Ambiguous when a quite different shift (≥ 1 h away) matches as many.
    let rival = scores.iter().skip(1).find(|(o, _)| (o - off).abs() >= 3600).map_or(0, |s| s.1);
    let needed = 2.min(targets.len());
    (best >= needed && best > rival).then_some((off, best))
}

fn fmt_offset(secs: i64) -> String {
    let sign = if secs < 0 { '−' } else { '+' };
    let m = secs.abs() / 60;
    if m % 60 == 0 {
        format!("{sign}{} h", m / 60)
    } else {
        format!("{sign}{}:{:02} h", m / 60, m % 60)
    }
}

/// Matches `targets` to `refs` (shifted by `offset`), taking the nearest
/// reference in time; when references both before and after are close,
/// they must agree on the place for the estimate to be confident.
fn match_with(
    targets: &[&Target],
    refs: &[&RefPoint],
    offset: i64,
    source: Source,
    max_gap: i64,
    sure_gap: i64,
    note: &dyn Fn(f64) -> String,
) -> Vec<Estimate> {
    let mut sorted: Vec<&RefPoint> = refs.to_vec();
    sorted.sort_by_key(|r| r.t);
    let times: Vec<i64> = sorted.iter().map(|r| r.t).collect();
    targets
        .iter()
        .filter_map(|tg| {
            let t = tg.t + offset;
            let j = nearest(&times, t)?;
            let gap = (times[j] - t).abs();
            if gap > max_gap {
                return None;
            }
            let r = sorted[j];
            // Neighbours on the other side in time: do they agree?
            let other = if times[j] <= t { sorted.get(j + 1).copied() } else { j.checked_sub(1).map(|k| sorted[k]) };
            let agree =
                other.is_none_or(|o| (o.t - t).abs() > max_gap || distance_km((r.lat, r.lon), (o.lat, o.lon)) < 1.5);
            let minutes = gap as f64 / 60.0;
            Some(Estimate {
                id: tg.id,
                lat: r.lat,
                lon: r.lon,
                source,
                gap_minutes: (minutes * 10.0).round() / 10.0,
                confident: gap <= sure_gap && agree,
                note: note(minutes) + if agree { "" } else { "; nearby photos disagree on the place" },
            })
        })
        .collect()
}

/// Estimates positions for `targets` from `photo_refs` (photos with GPS)
/// and `track` (imported location history). Returns estimates for the
/// targets it could place, plus notes about detected camera clock offsets.
pub fn estimate(targets: &[Target], photo_refs: &[RefPoint], track: &[RefPoint]) -> (Vec<Estimate>, Vec<String>) {
    const MIN: i64 = 60;
    let mut out: Vec<Estimate> = Vec::new();
    let mut notes = Vec::new();
    let mut by_camera: HashMap<Option<String>, Vec<&Target>> = HashMap::new();
    for t in targets {
        by_camera.entry(t.camera.clone()).or_default().push(t);
    }
    let mut cameras: Vec<_> = by_camera.into_iter().collect();
    cameras.sort_by(|a, b| a.0.cmp(&b.0));
    for (camera, group) in cameras {
        let cam_name = camera.clone().unwrap_or_else(|| "unknown camera".into());
        let mut left: Vec<&Target> = group;

        // 1. Same camera, minutes apart: same clock, no offset.
        let same: Vec<&RefPoint> = photo_refs.iter().filter(|r| camera.is_some() && r.camera == camera).collect();
        let found = match_with(&left, &same, 0, Source::Nearby, 30 * MIN, 10 * MIN, &|m| {
            format!("from a photo by the same camera {} apart", minutes_text(m))
        });
        left.retain(|t| !found.iter().any(|e| e.id == t.id));
        out.extend(found);

        // 2./3. Phones and location history: one clock offset per camera,
        // detected against both (they share local time) from all of this
        // camera's photos, then used for both.
        let others: Vec<&RefPoint> = photo_refs.iter().filter(|r| r.camera != camera).collect();
        if left.is_empty() || (others.is_empty() && track.is_empty()) {
            continue;
        }
        let mut all: Vec<i64> = others.iter().map(|r| r.t).chain(track.iter().map(|r| r.t)).collect();
        all.sort_unstable();
        let tt: Vec<i64> = left.iter().map(|t| t.t).collect();
        let Some((off, n)) = best_offset(&tt, &all, 20 * MIN) else {
            notes.push(format!("{cam_name}: couldn't work out its clock against the phone / location history"));
            continue;
        };
        let clock = if off == 0 { String::new() } else { format!(", camera clock {}", fmt_offset(-off)) };
        if off != 0 {
            notes.push(format!(
                "{cam_name}: clock is {} from local time ({n} of {} photos line up)",
                fmt_offset(-off),
                tt.len()
            ));
        }
        let found = match_with(&left, &others, off, Source::Phone, 45 * MIN, 15 * MIN, &|m| {
            format!("from a phone photo {} apart{clock}", minutes_text(m))
        });
        left.retain(|t| !found.iter().any(|e| e.id == t.id));
        out.extend(found);
        if !left.is_empty() && !track.is_empty() {
            let refs: Vec<&RefPoint> = track.iter().collect();
            out.extend(match_with(&left, &refs, off, Source::Timeline, 60 * MIN, 15 * MIN, &|m| {
                format!("from your location history {} apart{clock}", minutes_text(m))
            }));
        }
    }
    (out, notes)
}

fn minutes_text(m: f64) -> String {
    if m < 1.0 {
        "under a minute".into()
    } else {
        format!("{} min", m.round())
    }
}

// --------------------------------------------------------------- timeline

/// Points from a Google Maps location-history export. Understands the
/// on-device "Timeline.json" (semanticSegments / rawSignals), the phone
/// "location-history.json" list, and the older Takeout "Records.json".
/// Times keep their local offset when the file states one (matching
/// camera-style local times); plain UTC times are used as they are, and the
/// clock-offset detection absorbs the difference.
pub fn parse_timeline(json: &serde_json::Value) -> Vec<(i64, f64, f64)> {
    let mut out = Vec::new();
    walk(json, &mut out);
    out.sort_by_key(|p| p.0);
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

fn walk(v: &serde_json::Value, out: &mut Vec<(i64, f64, f64)>) {
    use serde_json::Value;
    match v {
        Value::Array(items) => items.iter().for_each(|i| walk(i, out)),
        Value::Object(o) => {
            let s = |k: &str| o.get(k).and_then(Value::as_str);
            // Records.json: latitudeE7 / longitudeE7 + timestamp.
            if let (Some(la), Some(lo)) =
                (o.get("latitudeE7").and_then(Value::as_f64), o.get("longitudeE7").and_then(Value::as_f64))
            {
                let t = s("timestamp")
                    .and_then(iso_secs)
                    .or_else(|| s("timestampMs").and_then(|ms| ms.parse::<i64>().ok()).map(|ms| ms / 1000));
                if let Some(t) = t {
                    out.push((t, la / 1e7, lo / 1e7));
                }
            }
            // rawSignals position: {"LatLng": "..", "timestamp": ".."}
            if let (Some(ll), Some(t)) = (s("LatLng").or(s("latLng")), s("timestamp").and_then(iso_secs)) {
                if let Some((la, lo)) = parse_latlng(ll) {
                    out.push((t, la, lo));
                }
            }
            // timelinePath point: {"point": "..", "time": ".."}
            if let (Some(p), Some(t)) = (s("point"), s("time").and_then(iso_secs)) {
                if let Some((la, lo)) = parse_latlng(p) {
                    out.push((t, la, lo));
                }
            }
            // A stay somewhere: sample it every 10 minutes.
            if let (Some(start), Some(end)) = (s("startTime").and_then(iso_secs), s("endTime").and_then(iso_secs)) {
                let place = o
                    .get("visit")
                    .and_then(|vi| vi.get("topCandidate"))
                    .and_then(|c| c.get("placeLocation"))
                    .and_then(|p| {
                        p.as_str()
                            .map(str::to_string)
                            .or_else(|| p.get("latLng").and_then(Value::as_str).map(str::to_string))
                    })
                    .and_then(|p| parse_latlng(&p));
                if let Some((la, lo)) = place {
                    let mut t = start;
                    while t <= end && end - start < 3 * 86_400 {
                        out.push((t, la, lo));
                        t += 600;
                    }
                    out.push((end, la, lo));
                }
                // Phone format "timelinePath": offsets in minutes from start.
                if let Some(Value::Array(path)) = o.get("timelinePath") {
                    for p in path {
                        let (Some(pt), Some(off)) = (
                            p.get("point").and_then(Value::as_str),
                            p.get("durationMinutesOffsetFromStartTime")
                                .and_then(|m| m.as_str().and_then(|m| m.parse::<i64>().ok()).or(m.as_i64())),
                        ) else {
                            continue;
                        };
                        if let Some((la, lo)) = parse_latlng(pt) {
                            out.push((start + off * 60, la, lo));
                        }
                    }
                }
            }
            for child in o.values() {
                walk(child, out);
            }
        }
        _ => {}
    }
}

/// `"-13.39°, -72.05°"`, `"geo:-13.39,-72.05"` or `"-13.39, -72.05"`.
pub fn parse_latlng(s: &str) -> Option<(f64, f64)> {
    let s = s.trim().trim_start_matches("geo:");
    let (a, b) = s.split_once(',')?;
    let num = |x: &str| x.trim().trim_end_matches('°').trim().parse::<f64>().ok();
    let (la, lo) = (num(a)?, num(b)?);
    ((-90.0..=90.0).contains(&la) && (-180.0..=180.0).contains(&lo)).then_some((la, lo))
}

/// ISO 8601 time → naive seconds of its *local* wall time when an offset
/// is given (`2026-10-01T10:00:00.000-05:00` → 10:00), else of UTC.
pub fn iso_secs(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.len() < 19 {
        return None;
    }
    naive_secs(&s[..19].replace('T', " "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> i64 {
        naive_secs(s).unwrap()
    }

    #[test]
    fn naive_time_round_trips_with_file_date() {
        assert_eq!(naive_secs("1970-01-01 00:00:00"), Some(0));
        assert_eq!(naive_secs("2024-02-29 12:34:56"), Some(1_709_210_096));
        assert_eq!(crate::workflow::file_date(t("2026-10-01 10:05:00")), "2026-10-01 10:05:00");
        assert_eq!(naive_secs("2026-13-01 00:00"), None);
    }

    fn photo(t_: &str, lat: f64, lon: f64, cam: &str) -> RefPoint {
        RefPoint { t: t(t_), lat, lon, camera: Some(cam.into()), asset_id: None }
    }

    #[test]
    fn camera_on_home_time_is_matched_to_phone_photos() {
        // Phone (local Peru time) and a camera still on Israel time (+8 h).
        let phone = vec![
            photo("2026-10-01 10:00:00", -13.3936, -72.0505, "Pixel"),
            photo("2026-10-01 10:40:00", -13.3936, -72.0505, "Pixel"),
            photo("2026-10-01 15:00:00", -13.5167, -71.9787, "Pixel"),
            photo("2026-10-02 09:00:00", -12.0896, -77.0438, "Pixel"),
        ];
        let targets = vec![
            Target { id: 1, t: t("2026-10-01 18:05:00"), camera: Some("D7500".into()) },
            Target { id: 2, t: t("2026-10-01 23:02:00"), camera: Some("D7500".into()) },
            Target { id: 3, t: t("2026-10-02 17:30:00"), camera: Some("D7500".into()) },
            Target { id: 4, t: t("2026-10-03 03:00:00"), camera: Some("D7500".into()) }, // nothing near
        ];
        let (est, notes) = estimate(&targets, &phone, &[]);
        let by = |id| est.iter().find(|e| e.id == id);
        assert_eq!(by(1).map(|e| (e.lat, e.source)), Some((-13.3936, Source::Phone)));
        assert!(by(1).unwrap().confident);
        assert_eq!(by(2).map(|e| e.lat), Some(-13.5167));
        assert!(by(3).is_some_and(|e| !e.confident), "30 min away: estimate, but flagged");
        assert!(by(4).is_none());
        assert!(notes.iter().any(|n| n.contains("+8 h")), "{notes:?}");
        assert!(by(1).unwrap().note.contains("camera clock +8 h"));
    }

    #[test]
    fn phone_and_history_share_one_camera_clock() {
        // Few phone photos, a location history covering other hours; only
        // together do they pin the camera clock (+8 h) unambiguously.
        let phone = vec![photo("2026-10-01 11:20:00", -13.39, -72.05, "Pixel")];
        let track: Vec<RefPoint> = ["2026-10-01 16:30:00", "2026-10-04 19:00:00"]
            .iter()
            .zip([(-13.51, -71.97), (-13.16, -72.54)])
            .map(|(when, (la, lo))| RefPoint { t: t(when), lat: la, lon: lo, camera: None, asset_id: None })
            .collect();
        let cam = |id, when: &str| Target { id, t: t(when), camera: Some("D7500".into()) };
        let targets = vec![cam(1, "2026-10-01 19:31:00"), cam(2, "2026-10-02 00:41:00"), cam(3, "2026-10-05 03:00:00")];
        let (est, _) = estimate(&targets, &phone, &track);
        let src = |id| est.iter().find(|e| e.id == id).map(|e| e.source);
        assert_eq!((src(1), src(2), src(3)), (Some(Source::Phone), Some(Source::Timeline), Some(Source::Timeline)));
    }

    #[test]
    fn same_camera_neighbours_and_disagreement() {
        let refs = vec![
            photo("2026-10-01 10:00:00", -13.39, -72.05, "D7500"),
            photo("2026-10-01 10:08:00", -12.08, -77.04, "D7500"),
        ];
        let targets = vec![
            Target { id: 1, t: t("2026-10-01 10:03:00"), camera: Some("D7500".into()) },
            Target { id: 2, t: t("2026-10-01 12:00:00"), camera: Some("D7500".into()) },
        ];
        let (est, _) = estimate(&targets, &refs, &[]);
        assert_eq!(est.len(), 1);
        assert_eq!(est[0].source, Source::Nearby);
        assert!(!est[0].confident, "the photos on either side are far apart");
    }

    #[test]
    fn timeline_formats_parse() {
        let new = serde_json::json!({"semanticSegments": [
            {"startTime": "2026-10-01T10:00:00.000-05:00", "endTime": "2026-10-01T10:30:00.000-05:00",
             "visit": {"topCandidate": {"placeLocation": {"latLng": "-13.3936°, -72.0505°"}}}},
            {"startTime": "2026-10-01T11:00:00.000-05:00", "endTime": "2026-10-01T12:00:00.000-05:00",
             "timelinePath": [{"point": "-13.40°, -72.00°", "time": "2026-10-01T11:10:00.000-05:00"}]}
        ], "rawSignals": [{"position": {"LatLng": "-13.5°, -71.9°", "timestamp": "2026-10-01T13:00:00.000-05:00"}}]});
        let pts = parse_timeline(&new);
        assert!(pts.contains(&(t("2026-10-01 10:00:00"), -13.3936, -72.0505)));
        assert!(pts.contains(&(t("2026-10-01 10:30:00"), -13.3936, -72.0505)));
        assert!(pts.contains(&(t("2026-10-01 11:10:00"), -13.40, -72.00)));
        assert!(pts.contains(&(t("2026-10-01 13:00:00"), -13.5, -71.9)));

        let phone = serde_json::json!([
            {"startTime": "2026-10-01T10:00:00.000-05:00", "endTime": "2026-10-01T11:00:00.000-05:00",
             "activity": {}, "timelinePath": [{"point": "geo:-13.39,-72.05", "durationMinutesOffsetFromStartTime": "20"}]}
        ]);
        assert_eq!(parse_timeline(&phone), vec![(t("2026-10-01 10:20:00"), -13.39, -72.05)]);

        let old = serde_json::json!({"locations": [{"latitudeE7": -133936000, "longitudeE7": -720505000, "timestamp": "2026-10-01T15:00:00Z"}]});
        assert_eq!(parse_timeline(&old), vec![(t("2026-10-01 15:00:00"), -13.3936, -72.0505)]);
        assert_eq!(parse_latlng("not a place"), None);
    }

    #[test]
    fn offset_needs_a_clear_winner() {
        // One target matching two very different shifts equally: ambiguous.
        let refs = vec![t("2026-10-01 10:00:00"), t("2026-10-01 20:00:00")];
        assert_eq!(best_offset(&[t("2026-10-01 15:00:00")], &refs, 600), None);
        let refs = vec![t("2026-10-01 10:00:00"), t("2026-10-01 13:00:00")];
        let targets = [t("2026-10-01 10:02:00"), t("2026-10-01 13:01:00")];
        assert_eq!(best_offset(&targets, &refs, 600).map(|r| r.0), Some(0));
    }
}
