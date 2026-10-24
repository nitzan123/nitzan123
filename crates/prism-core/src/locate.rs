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
    /// The nearest named place (museum, park…) to the photo's own GPS.
    /// Accepted as is (the user can still correct it); GPS drift can
    /// occasionally pick the building next door.
    Venue,
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
    /// Timeline only: the phone was staying at a place here (a museum,
    /// restaurant…), not travelling, with the place's name when the export
    /// has it.
    pub stop: Option<Stop>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct Stop {
    pub name: Option<String>,
}

/// One point of an imported location history.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackPoint {
    pub t: i64,
    pub lat: f64,
    pub lon: f64,
    pub stop: Option<Stop>,
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
    /// Taken during a stop in the location history: the position is the
    /// place itself, so it can be named (museum, restaurant…).
    pub stop: Option<Stop>,
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
            // While the phone stayed at a place, the stop (the place
            // itself) beats a raw GPS fix that happens to be closer in time.
            let near = |k: usize| (times[k] - t).abs() <= 10 * 60;
            let stop_at = (j.saturating_sub(4)..(j + 5).min(sorted.len()))
                .filter(|&k| near(k) && sorted[k].stop.is_some())
                .min_by_key(|&k| (times[k] - t).abs());
            let j = stop_at.unwrap_or(j);
            let gap = if stop_at.is_some() { (times[j] - t).abs() } else { gap };
            let r = sorted[j];
            // Neighbours on the other side in time: do they agree?
            let other = if times[j] <= t { sorted.get(j + 1).copied() } else { j.checked_sub(1).map(|k| sorted[k]) };
            let agree =
                other.is_none_or(|o| (o.t - t).abs() > max_gap || distance_km((r.lat, r.lon), (o.lat, o.lon)) < 1.5);
            let minutes = gap as f64 / 60.0;
            // Inside a stay the history has a point every 10 minutes.
            let stop = r.stop.clone().filter(|_| gap <= 10 * 60);
            Some(Estimate {
                id: tg.id,
                lat: r.lat,
                lon: r.lon,
                source,
                gap_minutes: (minutes * 10.0).round() / 10.0,
                confident: gap <= sure_gap && agree,
                note: note(minutes) + if agree { "" } else { "; nearby photos disagree on the place" },
                stop,
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
        let mut phone: Vec<i64> = others.iter().map(|r| r.t).collect();
        phone.sort_unstable();
        let mut all: Vec<i64> = phone.iter().copied().chain(track.iter().map(|r| r.t)).collect();
        all.sort_unstable();
        let tt: Vec<i64> = left.iter().map(|t| t.t).collect();
        // Phone photos are sparse, so they pin the clock best; a full
        // location history has points at almost every hour and can only
        // help when there are few phone photos.
        let detected = best_offset(&tt, &phone, 20 * MIN).or_else(|| best_offset(&tt, &all, 20 * MIN));
        let (off, n) = match detected {
            Some(found) => found,
            // Every shift lines up with a dense history: assume the camera
            // shows local time (the places stay marked to check).
            None if !track.is_empty() => {
                notes.push(format!(
                    "{cam_name}: couldn't check its clock against phone photos; assumed it shows local time"
                ));
                (0, 0)
            }
            None => {
                notes.push(format!("{cam_name}: couldn't work out its clock against the phone / location history"));
                continue;
            }
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
/// "location-history.json" list, the Takeout "Semantic Location History"
/// and the older "Records.json".
///
/// Times are returned as camera-style local times of the place where they
/// were recorded. The on-device export writes every time in the phone's
/// *current* zone (e.g. Israel, +03:00, for a trip to Peru) and states the
/// real local offset per segment (`startTimeTimezoneUtcOffsetMinutes`);
/// that offset is used for the segment and for raw points and paths inside
/// or next to it. Times without any zone information are used as stated,
/// and the clock-offset detection absorbs the difference.
pub fn parse_timeline(json: &serde_json::Value) -> Vec<TrackPoint> {
    let mut raw = Vec::new();
    let mut zones = Vec::new();
    walk(json, &mut raw, &mut zones);
    zones.sort_unstable();
    let zone_at = |utc: i64| -> Option<i64> {
        // The segment covering this moment, else the nearest within 6 h
        // (the shortest one if several).
        zones
            .iter()
            .map(|&(a, b, off)| (if utc < a { a - utc } else { (utc - b).max(0) }, b - a, off))
            .filter(|&(d, _, _)| d <= 6 * 3600)
            .min_by_key(|&(d, len, _)| (d, len))
            .map(|(_, _, off)| off)
    };
    let mut out: Vec<TrackPoint> = raw
        .into_iter()
        .map(|p| {
            let off = p.zone.or_else(|| zone_at(p.utc)).unwrap_or(p.stated);
            TrackPoint { t: p.utc + off, lat: p.lat, lon: p.lon, stop: p.stop }
        })
        .collect();
    // A stop wins over a raw position at the same second.
    out.sort_by_key(|p| (p.t, p.stop.is_none()));
    out.dedup_by(|a, b| a.t == b.t);
    out
}

/// A point as found in the file: UTC time, the offset its timestamp was
/// written in, and the real local offset when the segment states one.
struct RawPoint {
    utc: i64,
    stated: i64,
    zone: Option<i64>,
    lat: f64,
    lon: f64,
    stop: Option<Stop>,
}

/// `(start_utc, end_utc, local_offset_secs)` of segments that state a zone.
type Zones = Vec<(i64, i64, i64)>;

fn raw((utc, stated): (i64, i64), (lat, lon): (f64, f64), zone: Option<i64>, stop: Option<Stop>) -> RawPoint {
    RawPoint { utc, stated, zone, lat, lon, stop }
}

/// A stay from `start` to `end` at one place, sampled every 10 minutes.
fn push_stop(
    out: &mut Vec<RawPoint>,
    (start, end): ((i64, i64), (i64, i64)),
    at: (f64, f64),
    zone: Option<i64>,
    name: Option<String>,
) {
    let (a, b) = (start.0, end.0);
    if b < a || b - a >= 3 * 86_400 {
        return;
    }
    let stop = Some(Stop { name });
    let mut t = a;
    while t <= b {
        out.push(raw((t, start.1), at, zone, stop.clone()));
        t += 600;
    }
    out.push(raw(end, at, zone, stop));
}

fn walk(v: &serde_json::Value, out: &mut Vec<RawPoint>, zones: &mut Zones) {
    use serde_json::Value;
    match v {
        Value::Array(items) => items.iter().for_each(|i| walk(i, out, zones)),
        Value::Object(o) => {
            let s = |k: &str| o.get(k).and_then(Value::as_str);
            // Records.json: latitudeE7 / longitudeE7 + timestamp.
            if let (Some(la), Some(lo)) =
                (o.get("latitudeE7").and_then(Value::as_f64), o.get("longitudeE7").and_then(Value::as_f64))
            {
                let t = s("timestamp")
                    .and_then(iso_utc)
                    .or_else(|| s("timestampMs").and_then(|ms| ms.parse::<i64>().ok()).map(|ms| (ms / 1000, 0)));
                if let Some(t) = t {
                    out.push(raw(t, (la / 1e7, lo / 1e7), None, None));
                }
            }
            // Older Takeout "Semantic Location History": a named place.
            if let Some(pv) = o.get("placeVisit") {
                let loc = pv.get("location");
                let e7 = |k: &str| loc.and_then(|l| l.get(k)).and_then(Value::as_f64).map(|v| v / 1e7);
                let ts = |k: &str| pv.get("duration").and_then(|d| d.get(k)).and_then(Value::as_str).and_then(iso_utc);
                if let (Some(la), Some(lo), Some(a), Some(b)) =
                    (e7("latitudeE7"), e7("longitudeE7"), ts("startTimestamp"), ts("endTimestamp"))
                {
                    let name = loc
                        .and_then(|l| l.get("name"))
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|n| !n.is_empty())
                        .map(str::to_string);
                    push_stop(out, (a, b), (la, lo), None, name);
                }
            }
            // rawSignals position: {"LatLng": "..", "timestamp": ".."}
            if let (Some(ll), Some(t)) = (s("LatLng").or(s("latLng")), s("timestamp").and_then(iso_utc)) {
                if let Some(at) = parse_latlng(ll) {
                    out.push(raw(t, at, None, None));
                }
            }
            // timelinePath point: {"point": "..", "time": ".."}
            if let (Some(p), Some(t)) = (s("point"), s("time").and_then(iso_utc)) {
                if let Some(at) = parse_latlng(p) {
                    out.push(raw(t, at, None, None));
                }
            }
            if let (Some(start), Some(end)) = (s("startTime").and_then(iso_utc), s("endTime").and_then(iso_utc)) {
                let zone = o.get("startTimeTimezoneUtcOffsetMinutes").and_then(Value::as_i64).map(|m| m * 60);
                // A trip summary ("timelineMemory") spans days and carries
                // the home zone, so it says nothing about local time.
                if let (Some(z), false) = (zone, o.contains_key("timelineMemory")) {
                    zones.push((start.0, end.0, z));
                }
                // A stay somewhere.
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
                if let Some(at) = place {
                    push_stop(out, (start, end), at, zone, None);
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
                        if let Some(at) = parse_latlng(pt) {
                            out.push(raw((start.0 + off * 60, start.1), at, zone, None));
                        }
                    }
                }
            }
            for child in o.values() {
                walk(child, out, zones);
            }
        }
        _ => {}
    }
}

/// Names Google uses for location-history exports: `Timeline.json` (phone,
/// "Export Timeline data"), `location-history.json` and Takeout's
/// `Records.json`, including copies like `Timeline (1).json`. Takeout's
/// monthly `2026_SEPTEMBER.json` files are too generic a name to look for;
/// those can still be imported by hand.
pub fn is_history_file_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.ends_with(".json")
        && (n.starts_with("timeline")
            || n.starts_with("location-history")
            || n.starts_with("location history")
            || n == "records.json")
}

/// The newest location-history file directly inside any of `dirs`, with
/// its modified time (seconds). Sub-folders are not searched.
pub fn find_history_file(dirs: &[std::path::PathBuf]) -> Option<(std::path::PathBuf, i64)> {
    dirs.iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_str().is_some_and(is_history_file_name))
        .filter_map(|e| {
            let md = e.metadata().ok()?;
            let mtime = md.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
            (md.is_file() && md.len() > 0).then(|| (e.path(), mtime))
        })
        .max_by_key(|(_, m)| *m)
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

/// ISO time → `(utc_secs, offset_secs)` where the offset is the one the
/// time was written in (`Z` or none → 0, so the time is taken as stated).
pub fn iso_utc(s: &str) -> Option<(i64, i64)> {
    let local = iso_secs(s)?;
    let rest = s.trim()[19..].trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    let off = match rest.as_bytes().first() {
        Some(sign @ (b'+' | b'-')) => {
            let hm = rest[1..].replace(':', "");
            let (h, m) = (hm.get(..2)?.parse::<i64>().ok()?, hm.get(2..4).unwrap_or("0").parse::<i64>().ok()?);
            (h * 3600 + m * 60) * if *sign == b'-' { -1 } else { 1 }
        }
        _ => 0,
    };
    Some((local - off, off))
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
        RefPoint { t: t(t_), lat, lon, camera: Some(cam.into()), asset_id: None, stop: None }
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
            .map(|(when, (la, lo))| RefPoint { t: t(when), lat: la, lon: lo, camera: None, asset_id: None, stop: None })
            .collect();
        let cam = |id, when: &str| Target { id, t: t(when), camera: Some("D7500".into()) };
        let targets = vec![cam(1, "2026-10-01 19:31:00"), cam(2, "2026-10-02 00:41:00"), cam(3, "2026-10-05 03:00:00")];
        let (est, _) = estimate(&targets, &phone, &track);
        let src = |id| est.iter().find(|e| e.id == id).map(|e| e.source);
        assert_eq!((src(1), src(2), src(3)), (Some(Source::Phone), Some(Source::Timeline), Some(Source::Timeline)));
    }

    #[test]
    fn times_written_in_home_zone_use_the_trips_local_time() {
        // The phone (back in Israel, +03:00) wrote a Peru visit at 15:00
        // Lima time as 23:00+03:00, with the real offset alongside.
        let j = serde_json::json!({"semanticSegments": [
            {"startTime": "2026-09-23T10:00:00.000+03:00", "endTime": "2026-10-04T20:00:00.000+03:00",
             "startTimeTimezoneUtcOffsetMinutes": 180, "endTimeTimezoneUtcOffsetMinutes": 180,
             "timelineMemory": {"trip": {"distanceFromOriginKms": 12776}}},
            {"startTime": "2026-09-23T23:00:00.000+03:00", "endTime": "2026-09-23T23:30:00.000+03:00",
             "startTimeTimezoneUtcOffsetMinutes": -300, "endTimeTimezoneUtcOffsetMinutes": -300,
             "visit": {"topCandidate": {"placeLocation": {"latLng": "-12.0727°, -77.0709°"}}}},
            {"startTime": "2026-09-23T23:30:00.000+03:00", "endTime": "2026-09-24T00:30:00.000+03:00",
             "timelinePath": [{"point": "-12.08°, -77.05°", "time": "2026-09-23T23:45:00.000+03:00"}]}
        ], "rawSignals": [{"position": {"LatLng": "-12.1°, -77.03°", "timestamp": "2026-09-24T00:10:00.000+03:00"}}]});
        let pts = parse_timeline(&j);
        let at = |when: &str| pts.iter().find(|p| p.t == t(when)).map(|p| (p.lat, p.stop.is_some()));
        assert_eq!(at("2026-09-23 15:00:00"), Some((-12.0727, true)));
        assert_eq!(at("2026-09-23 15:45:00"), Some((-12.08, false)), "path next to the visit uses its zone");
        assert_eq!(at("2026-09-23 16:10:00"), Some((-12.1, false)), "raw signal too");
        assert_eq!(iso_utc("2026-09-23T23:00:00.000+03:00"), Some((t("2026-09-23 20:00:00"), 3 * 3600)));
        assert_eq!(iso_utc("2026-09-23T20:00:00Z"), Some((t("2026-09-23 20:00:00"), 0)));
    }

    #[test]
    fn finds_the_newest_history_export() {
        assert!(is_history_file_name("Timeline.json"));
        assert!(is_history_file_name("Timeline (1).json"));
        assert!(is_history_file_name("location-history.json"));
        assert!(is_history_file_name("Records.json"));
        assert!(!is_history_file_name("settings.json"));
        assert!(!is_history_file_name("Timeline.txt"));
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::write(a.path().join("Timeline.json"), "{}").unwrap();
        std::fs::write(a.path().join("notes.json"), "{}").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(b.path().join("Timeline (1).json"), "{}").unwrap();
        let found = find_history_file(&[a.path().into(), b.path().into(), "/no/such/dir".into()]).unwrap();
        assert_eq!(found.0, b.path().join("Timeline (1).json"));
        assert_eq!(find_history_file(&[]), None);
    }

    #[test]
    fn dense_history_alone_assumes_local_camera_time() {
        // A point every 5 minutes for two days: any clock shift "matches".
        let track: Vec<RefPoint> = (0..576)
            .map(|i| RefPoint {
                t: t("2026-09-23 00:00:00") + i * 300,
                lat: -12.0 - i as f64 * 1e-4,
                lon: -77.0,
                camera: None,
                asset_id: None,
                stop: None,
            })
            .collect();
        let cam = |id, when: &str| Target { id, t: t(when), camera: Some("D7500".into()) };
        let (est, notes) = estimate(&[cam(1, "2026-09-23 10:01:00"), cam(2, "2026-09-24 15:31:00")], &[], &track);
        assert_eq!(est.len(), 2);
        assert!(est.iter().all(|e| e.source == Source::Timeline));
        let e1 = est.iter().find(|e| e.id == 1).unwrap();
        assert!((e1.lat - (-12.0 - 120.0 * 1e-4)).abs() < 1e-9, "matched at local 10:00, not shifted");
        assert!(notes.iter().any(|n| n.contains("assumed it shows local time")));
    }

    #[test]
    fn photos_during_a_stop_get_the_place() {
        let track: Vec<RefPoint> = parse_timeline(&serde_json::json!({"timelineObjects": [
            {"placeVisit": {"location": {"latitudeE7": -120727000, "longitudeE7": -770709000, "name": "Museo Larco"},
              "duration": {"startTimestamp": "2026-09-23T14:00:00Z", "endTimestamp": "2026-09-23T15:00:00Z"}}},
            {"latitudeE7": -121000000, "longitudeE7": -770300000, "timestamp": "2026-09-23T16:30:00Z"}]}))
        .into_iter()
        .map(|p| RefPoint { t: p.t, lat: p.lat, lon: p.lon, camera: None, asset_id: None, stop: p.stop })
        .collect();
        let cam = |id, when: &str| Target { id, t: t(when), camera: Some("D7500".into()) };
        // A raw GPS fix closer in time than the stop's samples.
        let mut track = track;
        track.push(RefPoint {
            t: t("2026-09-23 14:13:00"),
            lat: -12.0728,
            lon: -77.0711,
            camera: None,
            asset_id: None,
            stop: None,
        });
        track.sort_by_key(|r| r.t);
        // Two photos inside the visit, one later on the road.
        let targets = vec![cam(1, "2026-09-23 14:12:00"), cam(2, "2026-09-23 14:47:00"), cam(3, "2026-09-23 16:31:00")];
        let (est, _) = estimate(&targets, &[], &track);
        let name = |id| est.iter().find(|e| e.id == id).and_then(|e| e.stop.clone()).and_then(|s| s.name);
        assert_eq!(name(1).as_deref(), Some("Museo Larco"));
        assert_eq!(name(2).as_deref(), Some("Museo Larco"));
        assert_eq!(est.iter().find(|e| e.id == 3).map(|e| e.stop.is_none()), Some(true));
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
        let pts: Vec<(i64, f64, f64, bool)> =
            parse_timeline(&new).into_iter().map(|p| (p.t, p.lat, p.lon, p.stop.is_some())).collect();
        assert!(pts.contains(&(t("2026-10-01 10:00:00"), -13.3936, -72.0505, true)));
        assert!(pts.contains(&(t("2026-10-01 10:30:00"), -13.3936, -72.0505, true)));
        assert!(pts.contains(&(t("2026-10-01 11:10:00"), -13.40, -72.00, false)));
        assert!(pts.contains(&(t("2026-10-01 13:00:00"), -13.5, -71.9, false)));

        let plain = |v: &serde_json::Value| -> Vec<(i64, f64, f64)> {
            parse_timeline(v).into_iter().map(|p| (p.t, p.lat, p.lon)).collect()
        };
        let phone = serde_json::json!([
            {"startTime": "2026-10-01T10:00:00.000-05:00", "endTime": "2026-10-01T11:00:00.000-05:00",
             "activity": {}, "timelinePath": [{"point": "geo:-13.39,-72.05", "durationMinutesOffsetFromStartTime": "20"}]}
        ]);
        assert_eq!(plain(&phone), vec![(t("2026-10-01 10:20:00"), -13.39, -72.05)]);

        let old = serde_json::json!({"locations": [{"latitudeE7": -133936000, "longitudeE7": -720505000, "timestamp": "2026-10-01T15:00:00Z"}]});
        assert_eq!(plain(&old), vec![(t("2026-10-01 15:00:00"), -13.3936, -72.0505)]);

        // Takeout Semantic Location History keeps the place's name.
        let semantic = serde_json::json!({"timelineObjects": [{"placeVisit": {
            "location": {"latitudeE7": -120727000, "longitudeE7": -770709000, "name": "Museo Larco"},
            "duration": {"startTimestamp": "2026-09-23T14:00:00Z", "endTimestamp": "2026-09-23T15:00:00Z"}}}]});
        let pts = parse_timeline(&semantic);
        assert_eq!(pts.len(), 7);
        assert!(pts.iter().all(|p| p.stop.as_ref().and_then(|s| s.name.as_deref()) == Some("Museo Larco")));
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
