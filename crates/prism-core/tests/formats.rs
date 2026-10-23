//! Modern phone/web formats: HEIC (iPhone), AVIF and JPEG XL. Fixtures are
//! 48x32 images, left half red and right half blue.

use prism_core::imaging::{is_supported, load_display, load_linear};
use prism_core::metadata::read_exif;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

fn is_red(p: [u8; 3]) -> bool {
    p[0] > 150 && p[2] < 110
}

fn is_blue(p: [u8; 3]) -> bool {
    p[2] > 150 && p[0] < 110
}

#[test]
fn heic_is_rotated_once_and_keeps_its_exif() {
    // Saved like an iPhone portrait: pixels stored landscape with an `irot`
    // transform AND EXIF Orientation=6. Applying both would turn it sideways.
    let p = fixture("rotated.heic");
    assert!(is_supported(&p));
    let img = load_display(&p, 0).unwrap();
    assert_eq!((img.width(), img.height()), (32, 48));
    assert!(is_red(img.get_pixel(16, 6).0), "top should be red: {:?}", img.get_pixel(16, 6));
    assert!(is_blue(img.get_pixel(16, 42).0), "bottom should be blue: {:?}", img.get_pixel(16, 42));

    let lin = load_linear(&p, 0).unwrap();
    assert_eq!((lin.width(), lin.height()), (32, 48));

    let exif = read_exif(&p);
    assert_eq!(exif.orientation, 6, "fixture must carry EXIF orientation like an iPhone");
    assert_eq!(exif.taken_at.as_deref(), Some("2025-09-23 10:11:12"));
    assert_eq!(exif.camera_model.as_deref(), Some("iPhone 15"));
}

#[test]
fn avif_and_jxl_decode() {
    for name in ["plain.avif", "plain.jxl"] {
        let p = fixture(name);
        assert!(is_supported(&p), "{name}");
        let img = load_display(&p, 0).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!((img.width(), img.height()), (48, 32), "{name}");
        assert!(is_red(img.get_pixel(6, 16).0), "{name} left: {:?}", img.get_pixel(6, 16));
        assert!(is_blue(img.get_pixel(42, 16).0), "{name} right: {:?}", img.get_pixel(42, 16));
        let thumb = load_display(&p, 24).unwrap();
        assert_eq!((thumb.width(), thumb.height()), (24, 16), "{name}");
    }
}

#[test]
fn misnamed_and_broken_files() {
    let dir = tempfile::tempdir().unwrap();
    // A PNG saved with a .jpg name (messaging apps do this) still opens.
    let png_as_jpg = dir.path().join("photo.jpg");
    image::RgbImage::from_pixel(10, 6, image::Rgb([1, 2, 3]))
        .save_with_format(&png_as_jpg, image::ImageFormat::Png)
        .unwrap();
    assert_eq!(load_display(&png_as_jpg, 0).unwrap().dimensions(), (10, 6));
    // Garbage in a modern container fails cleanly.
    for name in ["bad.heic", "bad.avif", "bad.jxl"] {
        let p = dir.path().join(name);
        std::fs::write(&p, b"not really an image").unwrap();
        assert!(load_display(&p, 0).is_err(), "{name}");
        assert!(load_linear(&p, 0).is_err(), "{name}");
        assert_eq!(read_exif(&p).orientation, 1);
    }
}
