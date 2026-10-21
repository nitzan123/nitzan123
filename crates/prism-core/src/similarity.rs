//! Perceptual difference hash (dHash). 64 bits; Hamming distance ≤ 10 is a
//! strong "same picture" signal (resized, recompressed, lightly edited).

use image::{imageops, GrayImage, RgbImage};

pub fn dhash(img: &RgbImage) -> u64 {
    let gray: GrayImage = imageops::grayscale(img);
    let small = imageops::resize(&gray, 9, 8, imageops::FilterType::Triangle);
    let mut hash = 0u64;
    for y in 0..8 {
        for x in 0..8 {
            let l = small.get_pixel(x, y).0[0];
            let r = small.get_pixel(x + 1, y).0[0];
            hash = (hash << 1) | u64::from(l > r);
        }
    }
    hash
}

pub fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(w: u32, h: u32, flip: bool) -> RgbImage {
        RgbImage::from_fn(w, h, |x, y| {
            let v = ((x * 255 / w) as u8).wrapping_add((y % 7) as u8 * 3);
            let v = if flip { 255 - v } else { v };
            image::Rgb([v, v / 2, 255 - v])
        })
    }

    #[test]
    fn resized_copy_is_close_and_inverse_is_far() {
        let a = dhash(&gradient(640, 480, false));
        let b = dhash(&imageops::resize(&gradient(640, 480, false), 160, 120, imageops::FilterType::Lanczos3));
        let c = dhash(&gradient(640, 480, true));
        assert!(hamming(a, b) <= 6, "resize distance {}", hamming(a, b));
        assert!(hamming(a, c) > 30, "inverse distance {}", hamming(a, c));
    }
}
