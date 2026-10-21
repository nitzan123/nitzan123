//! Headless develop: `cargo run --release --example develop_cli -- <out_dir> <files…>`
//!
//! For each file prints EXIF, then writes `<name>.preview.jpg` (embedded /
//! display path), `<name>.raw.jpg` (decoded, no adjustments) and
//! `<name>.auto.jpg` (auto settings) into `out_dir`. Handy for checking
//! decoder and pipeline output without the GUI.

use prism_core::develop::{auto::auto_settings, render_to_rgb8};
use prism_core::imaging::{load_display, load_linear};
use prism_core::metadata::read_exif;
use prism_core::DevelopSettings;
use std::path::PathBuf;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let out = PathBuf::from(args.next().ok_or("usage: develop_cli <out_dir> <files…>")?);
    std::fs::create_dir_all(&out)?;
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
    }
    Ok(())
}
