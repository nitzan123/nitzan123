//! End-to-end robustness: a library full of awkward files must index,
//! thumbnail, decode and render without panicking, and bad files must fail
//! individually with an error instead of taking the batch down.

use image::{DynamicImage, ImageBuffer, Luma, Rgb, Rgba};
use prism_core::catalog::Upsert;
use prism_core::develop::{masks::Mask, render_to_rgb8, CropRect, LocalAdjustment};
use prism_core::imaging::{load_display, load_linear};
use prism_core::thumbs::ThumbCache;
use prism_core::{scan, Catalog, DevelopSettings, SearchQuery};
use std::path::{Path, PathBuf};

struct Corpus {
    _dir: tempfile::TempDir,
    root: PathBuf,
    good: Vec<PathBuf>,
    bad: Vec<PathBuf>,
}

fn jpeg_with_orientation(path: &Path, w: u32, h: u32, orientation: u16) {
    // Minimal EXIF APP1 with a single Orientation tag (little-endian TIFF).
    let img = image::RgbImage::from_fn(w, h, |x, y| Rgb([(x * 255 / w) as u8, (y * 255 / h) as u8, 80]));
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 85).encode_image(&img).unwrap();
    let mut tiff = b"II*\0\x08\0\0\0".to_vec();
    tiff.extend_from_slice(&1u16.to_le_bytes()); // 1 entry
    tiff.extend_from_slice(&0x0112u16.to_le_bytes()); // Orientation
    tiff.extend_from_slice(&3u16.to_le_bytes()); // SHORT
    tiff.extend_from_slice(&1u32.to_le_bytes());
    tiff.extend_from_slice(&orientation.to_le_bytes());
    tiff.extend_from_slice(&[0, 0]);
    tiff.extend_from_slice(&0u32.to_le_bytes()); // no next IFD
    let mut app1 = b"Exif\0\0".to_vec();
    app1.extend_from_slice(&tiff);
    let mut out = jpeg[..2].to_vec(); // SOI
    out.extend_from_slice(&[0xFF, 0xE1]);
    out.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(&app1);
    out.extend_from_slice(&jpeg[2..]);
    std::fs::write(path, out).unwrap();
}

fn build() -> Corpus {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Library with spaces");
    let sub = root.join("טיול לים"); // Hebrew folder name
    std::fs::create_dir_all(&sub).unwrap();
    let mut good = Vec::new();
    let mut bad = Vec::new();
    let mut add = |p: PathBuf, ok: bool| if ok { good.push(p) } else { bad.push(p) };

    let p = sub.join("תמונה ים.jpg");
    image::RgbImage::from_pixel(640, 480, Rgb([30, 120, 200])).save(&p).unwrap();
    add(p, true);

    let p = root.join("portrait_rotated.jpg");
    jpeg_with_orientation(&p, 400, 300, 6);
    add(p, true);

    let p = root.join("alpha.png");
    ImageBuffer::from_pixel(300, 200, Rgba([200u8, 50, 50, 0])).save(&p).unwrap();
    add(p, true);

    let p = root.join("deep16.tif");
    DynamicImage::ImageRgb16(ImageBuffer::from_fn(256, 128, |x, _| Rgb([(x * 256) as u16, 40000, 1000])))
        .save(&p)
        .unwrap();
    add(p, true);

    let p = root.join("gray.jpg");
    ImageBuffer::from_fn(120, 90, |x, _| Luma([(x * 2) as u8])).save(&p).unwrap();
    add(p, true);

    let p = root.join("tiny.png");
    image::RgbImage::from_pixel(1, 1, Rgb([255, 255, 255])).save(&p).unwrap();
    add(p, true);

    let p = root.join("UPPER.JPG");
    image::RgbImage::from_pixel(50, 2000, Rgb([10, 10, 10])).save_with_format(&p, image::ImageFormat::Jpeg).unwrap();
    add(p, true);

    let p = root.join("empty.jpg");
    std::fs::write(&p, b"").unwrap();
    add(p, false);

    let p = root.join("text_pretending.jpg");
    std::fs::write(&p, b"definitely not a jpeg").unwrap();
    add(p, false);

    let p = root.join("truncated.jpg");
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut bytes)
        .encode_image(&image::RgbImage::from_pixel(400, 400, Rgb([1, 2, 3])))
        .unwrap();
    std::fs::write(&p, &bytes[..bytes.len() / 3]).unwrap();
    // Truncated JPEGs may decode partially (grey bottom) or fail — both fine.

    let p = root.join("fake.nef");
    std::fs::write(&p, vec![0u8; 5000]).unwrap();
    add(p, false);

    std::fs::write(root.join("notes.txt"), "ignored").unwrap();
    Corpus { _dir: dir, root, good, bad }
}

#[test]
fn awkward_library_indexes_and_renders() {
    let c = build();
    let paths = scan::list_media(&c.root, true);
    assert!(!paths.iter().any(|p| p.extension().is_some_and(|e| e == "txt")));
    let scanned = scan::scan_files(&paths);
    assert_eq!(scanned.len(), paths.len(), "stat/EXIF must not drop files");

    let mut cat = Catalog::open(&c.root.join(".catalog/catalog.sqlite")).unwrap();
    let res = cat.upsert_many(&scanned).unwrap();
    assert!(res.iter().all(|r| matches!(r, Upsert::Inserted(_))));

    // Unicode search and folder scoping work.
    let hits = cat.search(&SearchQuery { text: Some("ים".into()), ..Default::default() }).unwrap();
    assert_eq!(hits.len(), 1);
    let folder = c.root.join("טיול לים").to_string_lossy().to_string();
    assert_eq!(cat.search(&SearchQuery { folder: Some(folder), ..Default::default() }).unwrap().len(), 1);

    let thumbs = ThumbCache::new(c.root.join(".thumbs")).unwrap();
    let mut s = DevelopSettings::default();
    s.light.exposure = 0.7;
    s.light.clarity = 40.0;
    s.mixer.saturation[5] = 30.0;
    s.curve.points = vec![[0.0, 0.05], [0.5, 0.55], [1.0, 0.95]];
    s.detail.sharpen_amount = 80.0;
    s.detail.noise_reduction = 40.0;
    s.vignette.amount = -40.0;
    s.geometry = prism_core::develop::Geometry {
        rotate90: 3,
        flip_horizontal: true,
        straighten: -7.5,
        crop: Some(CropRect { x: 0.1, y: 0.1, w: 0.8, h: 0.8 }),
    };
    s.local.push(LocalAdjustment { mask: Mask::Sky { sensitivity: 60.0 }, exposure: -0.5, ..Default::default() });
    s.local.push(LocalAdjustment {
        mask: Mask::Radial { cx: 0.5, cy: 0.5, rx: 0.3, ry: 0.3, feather: 50.0 },
        invert: true,
        blur: 40.0,
        luma_range: Some([0.2, 0.8]),
        ..Default::default()
    });

    for p in &c.good {
        let sf = scanned.iter().find(|f| &f.path == p).expect("scanned");
        thumbs.ensure(p, sf.mtime, sf.size).unwrap_or_else(|e| panic!("thumb {p:?}: {e}"));
        let d = load_display(p, 512).unwrap_or_else(|e| panic!("display {p:?}: {e}"));
        assert!(d.width() >= 1 && d.height() >= 1);
        let lin = load_linear(p, 512).unwrap_or_else(|e| panic!("linear {p:?}: {e}"));
        for settings in [DevelopSettings::default(), s.clone()] {
            let out = render_to_rgb8(&lin, &settings);
            assert!(out.width() >= 1 && out.height() >= 1, "{p:?}");
        }
    }
    for p in &c.bad {
        assert!(load_display(p, 512).is_err(), "{p:?} should fail cleanly");
        assert!(load_linear(p, 512).is_err(), "{p:?} should fail cleanly");
    }

    // EXIF orientation 6 (rotate 90° CW): 400x300 stored → 300x400 shown.
    let d = load_display(&c.root.join("portrait_rotated.jpg"), 0).unwrap();
    assert_eq!((d.width(), d.height()), (300, 400));
}

#[test]
fn prune_removes_deleted_files() {
    let c = build();
    let mut cat = Catalog::open_in_memory().unwrap();
    let scanned = scan::scan_files(&scan::list_media(&c.root, true));
    cat.upsert_many(&scanned).unwrap();
    let before = cat.search(&SearchQuery::default()).unwrap().len();
    std::fs::remove_file(c.root.join("gray.jpg")).unwrap();
    assert_eq!(cat.prune_missing(&c.root.to_string_lossy()).unwrap(), 1);
    assert_eq!(cat.search(&SearchQuery::default()).unwrap().len(), before - 1);

    // Deleting a whole sub-folder: its rows go when the parent is rescanned.
    std::fs::remove_dir_all(c.root.join("טיול לים")).unwrap();
    let root = c.root.to_string_lossy();
    assert_eq!(cat.prune_under(&root, false).unwrap(), 0, "non-recursive leaves sub-folders alone");
    assert_eq!(cat.prune_under(&format!("{root}/"), true).unwrap(), 1);
    assert_eq!(cat.search(&SearchQuery::default()).unwrap().len(), before - 2);
}
