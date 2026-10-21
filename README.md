# Prism Studio

A fast desktop photo manager and RAW editor that never changes your original
files. You can browse, rate, tag and search a large library, develop RAW
files with live sliders, and make masked local edits. Everything is stored in
a catalog and in standard XMP sidecars.

Built with **Rust + Tauri 2**. Runs on Windows, macOS and Linux.

The feature set takes inspiration from ACDSee Photo Studio. Every line of
code is original — see [docs/reverse-engineering-findings.md](docs/reverse-engineering-findings.md)
for what was studied and where the clean-room boundary sits.

| Manage | Develop | Edit (local masks) |
|---|---|---|
| ![Manage](docs/screenshots/manage.jpg) | ![Develop](docs/screenshots/develop.jpg) | ![Edit](docs/screenshots/edit.jpg) |

## Features

**Manage**
- Add folders (recursive). A background indexer reads EXIF and builds
  thumbnails (RAW files use their embedded preview), then computes
  perceptual hashes.
- Virtualised grid that stays fast with large libraries. Supports multi-select
  (click, Ctrl/⌘, Shift) and Ctrl+A.
- Ratings (`0`–`5`), colour labels (`6`–`9`), tagged (`T`), captions,
  keywords and collections.
- Filters: rating, label, tagged, developed, date range, camera and free text
  (matches file name, caption and keywords). Searches can be saved.
- **Find similar**: near-duplicate detection with a 64-bit dHash.
- Sorting by name, capture date, rating, size or modified date.

**View**: full-screen viewer with filmstrip, wheel or arrow-key navigation,
and 1:1 zoom with panning (`Z` or click).

**Develop** (non-destructive, live preview, histogram):
- White balance, exposure, contrast, highlights, shadows, whites, blacks,
  clarity, vibrance and saturation.
- **8-band colour mixer** (hue, saturation and luminance), tone curve
  (monotone cubic), split toning, sharpening, noise reduction and vignette.
- Geometry: rotate, flip, straighten (auto-scales so no blank corners
  appear) and crop with aspect presets.
- **Auto tone**, a separate conservative **Auto WB**, before/after (`\`),
  undo/redo (Ctrl+Z / Ctrl+Shift+Z), and copy/paste of settings to a
  selection. Edits autosave.
- Real RAW decoding from sensor data (rawloader). Tested on Nikon NEF and
  Canon CR2.

**Edit**: local adjustments, each with its own exposure, contrast,
saturation, temperature, tint and blur.
- Masks: radial, linear gradient, **sky (auto-detect)** and whole image. Any
  mask can be inverted or limited to a brightness range.
- Drag the handles on the photo to place a mask.
- Quick looks: Brighten sky, Dramatic sky, Blur background, Spotlight,
  Warm foreground.

**Export**: full-size or resized JPEG with your develop settings applied.

## File formats

| | |
|---|---|
| Raster | JPEG, PNG, TIFF, WebP, GIF, BMP |
| RAW | CR2, NEF, ARW, DNG, ORF, RW2, RAF, PEF, SRW and [many more](crates/prism-core/src/raw.rs) via rawloader. CR3 has a thumbnail preview only. |
| Sidecar | `photo.ext.xmp`, which holds `xmp:Rating`, `xmp:Label`, `dc:subject` and `dc:description`. Lightroom, darktable and digiKam can read these. Develop settings are stored in the `prism:` namespace. |

## Build and run

Requirements: Rust (stable) and the
[Tauri 2 system prerequisites](https://v2.tauri.app/start/prerequisites/)
(on Linux: `libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev librsvg2-dev`).

```bash
cargo run -p prism-studio --release                # start the app
cargo run -p prism-studio --release -- ~/Pictures  # start and index a folder
cargo test --workspace                             # 40 unit tests
```

To build installers (`.msi`/`.exe`, `.dmg`, `.deb`/`.AppImage`):

```bash
cargo install tauri-cli --version "^2"
cd src-tauri && cargo tauri build
```

There is also a headless develop tool, useful for checking decoder and
pipeline output:

```bash
cargo run --release --example develop_cli -- out/ photo1.NEF photo2.CR2
```

## Architecture

```
crates/prism-core   UI-agnostic engine (all logic, all tests)
  catalog.rs        SQLite catalog: assets, folders, keywords, collections, saved searches, develop JSON
  scan.rs           folder walk + EXIF (parallel)
  imaging.rs        single entry point for decoding (display path / linear path)
  raw.rs            embedded-preview extraction, RAW decode → linear sRGB, base curve
  thumbs.rs         content-keyed on-disk thumbnail cache (+ developed variants)
  similarity.rs     dHash + Hamming distance
  xmp.rs            sidecar read/write
  develop/          settings model, render pipeline, masks, auto
src-tauri           Tauri shell: commands, background indexer, prism:// image protocol
ui                  vanilla ES modules, no build step
```

Render pipeline order (see `develop/pipeline.rs`):
geometry → WB / exposure (linear) → tone (highlights, shadows, whites,
blacks, contrast with roll-off) → clarity → display sRGB → tone curve →
colour mixer, vibrance, saturation → split tone → local masks → vignette →
noise reduction and sharpening.

## Known limitations (v0.1)

- RAW demosaic uses 2×2 superpixel binning, so RAW output is half the sensor
  resolution. Full-resolution demosaic (AMaZE/RCD-class) is the top roadmap
  item.
- No lens-profile correction or chromatic-aberration removal yet.
- The sky mask is a heuristic (colour, brightness and connection to the top
  edge), not ML. The mask-provider interface (`develop/masks.rs`) is where
  ONNX segmentation would plug in.
- No face recognition or AI keywording yet.
- Exports are JPEG only. HEIC and CR3 full decode are not supported.

See [docs/architecture.md](docs/architecture.md) for the roadmap.

## Credits

The RAW sample files used in screenshots and testing come from
[raw.pixls.us](https://raw.pixls.us) (CC0).
