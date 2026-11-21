//! Bounded raster decoding for pages and thumbnails.

use std::io::Cursor;

use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits};
use jpeg_decoder::{CodingProcess, Decoder as JpegDecoder, ImageInfo, PixelFormat};

mod cover;

pub use cover::{derive_cover, CoverVariants};

pub const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_OUTPUT_EDGE: u32 = 1600;
const MAX_INPUT_EDGE: u32 = 8000;
const MAX_BITMAP_PIXELS: u64 = 8_000_000;
const MAX_PROGRESSIVE_SOURCE_PIXELS: u64 = 8_000_000;
const MAX_JPEG_SOURCE_PIXELS: u64 = 32_000_000;
const MAX_CODEC_ALLOCATION: usize = 40 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("图片文件超过 32 MiB 上限")]
    InputTooLarge,
    #[error("图片尺寸超出内存预算（{width} × {height}）")]
    Dimensions { width: u32, height: u32 },
    #[error("显示尺寸必须介于 1 到 1600 像素之间")]
    InvalidOutputEdge,
    #[error("无法识别图片，请使用 JPEG、PNG、GIF 或 WebP")]
    UnsupportedFormat,
    #[error("图片解码失败：{0}")]
    Image(#[from] image::ImageError),
    #[error("JPEG 解码失败：{0}")]
    Jpeg(#[from] jpeg_decoder::Error),
    #[error("图片像素数据与尺寸不符")]
    InvalidBuffer,
    #[error("内存不足，无法读取图片")]
    Allocation(#[from] std::collections::TryReserveError),
}

/// Decode a single still page. `max_edge` is a display target, not a source size.
/// The returned RGBA buffer is at most `max_edge * max_edge * 4` bytes.
pub fn decode_page(bytes: &[u8], max_edge: u32) -> Result<RgbaImage, MediaError> {
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(MediaError::InputTooLarge);
    }
    if !(1..=MAX_OUTPUT_EDGE).contains(&max_edge) {
        return Err(MediaError::InvalidOutputEdge);
    }
    let format = image::guess_format(bytes).map_err(|_| MediaError::UnsupportedFormat)?;
    let (bitmap, orientation) = match format {
        ImageFormat::Jpeg => decode_jpeg(bytes, max_edge)?,
        ImageFormat::Png | ImageFormat::Gif | ImageFormat::WebP => decode_bitmap(bytes, format)?,
        _ => return Err(MediaError::UnsupportedFormat),
    };
    let (width, height) = fit(bitmap.width(), bitmap.height(), max_edge);
    let mut bitmap = if (width, height) == (bitmap.width(), bitmap.height()) {
        bitmap
    } else {
        bitmap.thumbnail_exact(width, height)
    };
    // Apply orientation after downsampling to keep rotations within the output budget.
    bitmap.apply_orientation(orientation);
    let rgba = bitmap.into_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(RgbaImage {
        width,
        height,
        pixels: rgba.into_raw(),
    })
}

fn decode_bitmap(
    bytes: &[u8],
    format: ImageFormat,
) -> Result<(DynamicImage, Orientation), MediaError> {
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut decoder_limits = Limits::default();
    decoder_limits.max_alloc = Some(MAX_CODEC_ALLOCATION as u64);
    reader.limits(decoder_limits.clone());
    let mut decoder = reader.into_decoder()?;
    let (width, height) = decoder.dimensions();
    check_dimensions(width, height, MAX_BITMAP_PIXELS)?;
    // Bound different bit depths too. Prefer caller-owned checks to decoder-specific defaults.
    decoder_limits.max_image_width = Some(MAX_INPUT_EDGE);
    decoder_limits.max_image_height = Some(MAX_INPUT_EDGE);
    decoder_limits.reserve(decoder.total_bytes())?;
    decoder.set_limits(decoder_limits)?;
    let orientation = decoder.orientation()?;
    Ok((DynamicImage::from_decoder(decoder)?, orientation))
}

fn decode_jpeg(bytes: &[u8], max_edge: u32) -> Result<(DynamicImage, Orientation), MediaError> {
    let mut decoder = JpegDecoder::new(Cursor::new(bytes));
    decoder.set_max_decoding_buffer_size(MAX_CODEC_ALLOCATION);
    decoder.read_info()?;
    let info = decoder.info().ok_or(MediaError::InvalidBuffer)?;
    let (original_width, original_height) = (u32::from(info.width), u32::from(info.height));
    // Progressive and lossless decoders can retain coefficients or samples at original size.
    if info.coding_process == CodingProcess::DctSequential {
        check_dimensions(original_width, original_height, MAX_JPEG_SOURCE_PIXELS)?;
    } else {
        check_dimensions(
            original_width,
            original_height,
            MAX_PROGRESSIVE_SOURCE_PIXELS,
        )?;
    }
    // Preserve the requested quality while leveraging DCT downsampling at 1/2, 1/4 or 1/8.
    if info.coding_process != CodingProcess::Lossless
        && original_width.max(original_height) > max_edge
    {
        let (width, height) = fit(original_width, original_height, max_edge);
        let (scaled_width, scaled_height) = decoder.scale(width as u16, height as u16)?;
        check_dimensions(scaled_width.into(), scaled_height.into(), MAX_BITMAP_PIXELS)?;
    }
    let decoded = decoder.decode()?;
    let info = decoder.info().ok_or(MediaError::InvalidBuffer)?;
    let orientation = decoder
        .exif_data()
        .and_then(|data| {
            Orientation::from_exif_chunk(data.strip_prefix(b"Exif\0\0").unwrap_or(data))
        })
        .unwrap_or(Orientation::NoTransforms);
    Ok((jpeg_bitmap(decoded, info)?, orientation))
}

fn jpeg_bitmap(decoded: Vec<u8>, info: ImageInfo) -> Result<DynamicImage, MediaError> {
    let (width, height) = (u32::from(info.width), u32::from(info.height));
    let count = width as usize * height as usize;
    if decoded.len() != count * info.pixel_format.pixel_bytes() {
        return Err(MediaError::InvalidBuffer);
    }
    let image = match info.pixel_format {
        PixelFormat::L8 => {
            image::GrayImage::from_raw(width, height, decoded).map(DynamicImage::ImageLuma8)
        }
        PixelFormat::RGB24 => {
            image::RgbImage::from_raw(width, height, decoded).map(DynamicImage::ImageRgb8)
        }
        PixelFormat::L16 => {
            let mut samples = Vec::new();
            samples.try_reserve_exact(count)?;
            samples.extend(
                decoded
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|sample| u16::from_ne_bytes([sample[0], sample[1]])),
            );
            image::ImageBuffer::from_raw(width, height, samples).map(DynamicImage::ImageLuma16)
        }
        PixelFormat::CMYK32 => {
            let mut rgb = Vec::new();
            rgb.try_reserve_exact(count * 3)?;
            for pixel in decoded.as_chunks::<4>().0 {
                let black = 255 - u16::from(pixel[3]);
                rgb.extend(
                    pixel[..3]
                        .iter()
                        .map(|&channel| ((255 - u16::from(channel)) * black / 255) as u8),
                );
            }
            image::RgbImage::from_raw(width, height, rgb).map(DynamicImage::ImageRgb8)
        }
    };
    image.ok_or(MediaError::InvalidBuffer)
}

fn check_dimensions(width: u32, height: u32, max_pixels: u64) -> Result<(), MediaError> {
    if width == 0
        || height == 0
        || width.max(height) > MAX_INPUT_EDGE
        || u64::from(width) * u64::from(height) > max_pixels
    {
        return Err(MediaError::Dimensions { width, height });
    }
    Ok(())
}

fn fit(width: u32, height: u32, max_edge: u32) -> (u32, u32) {
    let edge = width.max(height);
    if edge <= max_edge {
        (width, height)
    } else {
        let scale = |dimension: u32| ((dimension * max_edge + edge / 2) / edge).max(1);
        (scale(width), scale(height))
    }
}
