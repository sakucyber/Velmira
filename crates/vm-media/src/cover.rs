use image::codecs::webp::WebPEncoder;
use image::imageops;
use image::{ExtendedColorType, RgbImage};

use crate::{decode_page, MediaError, MAX_OUTPUT_EDGE};

const SHELF_WIDTH: u32 = 400;
const DETAIL_WIDTH: u32 = 800;

/// Opaque, lossless WebP cover art in 400×600 (shelf) and 800×1200 (detail).
#[derive(Debug)]
pub struct CoverVariants {
    pub shelf_webp: Vec<u8>,
    pub detail_webp: Vec<u8>,
}

/// Decode once, correct orientation, center-crop, and flatten transparency onto white.
pub fn derive_cover(bytes: &[u8]) -> Result<CoverVariants, MediaError> {
    let rgba = decode_page(bytes, MAX_OUTPUT_EDGE)?;
    let image = image::RgbaImage::from_raw(rgba.width, rgba.height, rgba.pixels)
        .ok_or(MediaError::InvalidBuffer)?;
    let (width, height) = cover_crop(image.width(), image.height());
    let x_offset = (image.width() - width) / 2;
    let y_offset = (image.height() - height) / 2;
    let crop = RgbImage::from_fn(width, height, |x, y| {
        let [red, green, blue, alpha] = image.get_pixel(x + x_offset, y + y_offset).0;
        let alpha = u16::from(alpha);
        let flatten =
            |channel| ((u16::from(channel) * alpha + 255 * (255 - alpha) + 127) / 255) as u8;
        image::Rgb([flatten(red), flatten(green), flatten(blue)])
    });
    drop(image);

    Ok(CoverVariants {
        shelf_webp: encode(&crop, SHELF_WIDTH)?,
        detail_webp: encode(&crop, DETAIL_WIDTH)?,
    })
}

fn cover_crop(width: u32, height: u32) -> (u32, u32) {
    let units = (width / 2).min(height / 3);
    if units > 0 {
        (units * 2, units * 3)
    } else {
        // Preserve a usable sample when a source is smaller than even one 2×3 unit.
        (width.min(2), height.min(3))
    }
}

fn encode(crop: &RgbImage, width: u32) -> Result<Vec<u8>, MediaError> {
    let height = 3 * width / 2;
    let variant = if width <= crop.width() && height <= crop.height() {
        imageops::thumbnail(crop, width, height)
    } else {
        // Keep the upscaling buffer to one output image. Bilinear sampling also
        // preserves solid colors and avoids resampling outside the crop.
        RgbImage::from_fn(width, height, |x, y| {
            let u = (x as f32 + 0.5) / width as f32;
            let v = (y as f32 + 0.5) / height as f32;
            imageops::sample_bilinear(crop, u, v).unwrap_or_else(|| *crop.get_pixel(0, 0))
        })
    };
    let mut webp = Vec::new();
    WebPEncoder::new_lossless(&mut webp).encode(
        variant.as_raw(),
        width,
        height,
        ExtendedColorType::Rgb8,
    )?;
    Ok(webp)
}
