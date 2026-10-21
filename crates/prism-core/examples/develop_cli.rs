//! Headless develop: `cargo run --release --example develop_cli -- <out_dir> <files…>`
//!
//! For each file prints EXIF, then writes `<name>.preview.jpg` (embedded /
//! display path), `<name>.raw.jpg` (decoded, no adjustments) and
//! `<name>.auto.jpg` (auto settings) into `out_dir`, plus
//! `<name>.caption.jpg` with a date/place caption (place via OpenStreetMap
//! when the file has GPS). Handy for checking output without the GUI.

use prism_core::develop::{auto::auto_settings, render_to_rgb8};
use prism_core::geocode::{format_coords, Geocoder};
use prism_core::imaging::{load_display, load_linear};
use prism_core::metadata::read_exif;
use prism_core::overlay::{caption_lines, draw_caption, Caption};
use prism_core::DevelopSettings;
use std::path::PathBuf;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let out = PathBuf::from(args.next().ok_or("usage: develop_cli <out_dir> <files…>")?);
    std::fs::create_dir_all(&out)?;
    let geocoder = Geocoder::new();
    for file in args.map(PathBuf::from) {
        let name = file.file_name().unwrap().to_string_lossy().to_string();
        let exif = read_exif(&file);
        println!("{name}: {exif:?}");

        let t = Instant::now();
        load_display(&file, 1600)?.save(out.join(format!("{name}.preview.jpg")))?;
        println!("  display path   {:>6} ms", t.elapsed().as_millis());

        let t = Instant::now();
        let lin = load_linear(&file, 2048)?;
        println!("  linear decode  {:>6} ms  {}x{}", t.elapsed().as_millis(), lin.width(), lin.height());

        let t = Instant::now();
        render_to_rgb8(&lin, &DevelopSettings::default()).save(out.join(format!("{name}.raw.jpg")))?;
        println!("  render (id)    {:>6} ms", t.elapsed().as_millis());

        let s = auto_settings(&lin);
        let t = Instant::now();
        render_to_rgb8(&lin, &s).save(out.join(format!("{name}.auto.jpg")))?;
        println!(
            "  render (auto)  {:>6} ms  exp {:+.2} temp {:+.0} tint {:+.0} hl {:.0} sh {:.0}",
            t.elapsed().as_millis(),
            s.light.exposure,
            s.white_balance.temperature,
            s.white_balance.tint,
            s.light.highlights,
            s.light.shadows
        );

        let mut img = render_to_rgb8(&load_linear(&file, 0)?, &DevelopSettings::default());
        let place = exif.gps.map(|[lat, lon]| match geocoder.reverse(lat, lon, "en") {
            Ok(Some(p)) => p,
            _ => format_coords(lat, lon),
        });
        let caption = Caption { custom_text: "(c) Prism Studio".into(), ..Default::default() };
        let lines = caption_lines(&caption, exif.taken_at.as_deref(), place.as_deref());
        draw_caption(&mut img, &lines, &caption);
        img.save(out.join(format!("{name}.caption.jpg")))?;
        println!("  caption        {lines:?}  ({}x{})", img.width(), img.height());
    }
    Ok(())
}
