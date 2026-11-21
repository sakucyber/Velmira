use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder, Rgba, RgbaImage};
use vm_media::{decode_page, derive_cover, MediaError, MAX_OUTPUT_EDGE};

fn png(image: &RgbaImage) -> Vec<u8> {
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            ExtendedColorType::Rgba8,
        )
        .unwrap();
    bytes
}

#[test]
fn wide_cover_crops_side_edges_and_composites_alpha() {
    let source = RgbaImage::from_fn(8, 6, |x, _| match x {
        0 | 1 => Rgba([0, 0, 255, 255]),
        6 | 7 => Rgba([0, 255, 0, 255]),
        _ => Rgba([255, 0, 0, 64]),
    });
    let cover = derive_cover(&png(&source)).unwrap();
    for (bytes, expected_size) in [
        (cover.shelf_webp, (400, 600)),
        (cover.detail_webp, (800, 1200)),
    ] {
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WEBP");
        let image = decode_page(&bytes, MAX_OUTPUT_EDGE).unwrap();
        assert_eq!((image.width, image.height), expected_size);
        assert!(image
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| *p == [255, 191, 191, 255]));
    }
}

#[test]
fn tall_cover_only_keeps_the_center_and_tiny_covers_still_work() {
    let source = RgbaImage::from_fn(4, 12, |_, y| match y {
        3..=8 => Rgba([10, 50, 200, 255]),
        _ => Rgba([255, 0, 0, 255]),
    });
    let cover = derive_cover(&png(&source)).unwrap();
    let image = decode_page(&cover.shelf_webp, MAX_OUTPUT_EDGE).unwrap();
    assert!(image
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .all(|p| *p == [10, 50, 200, 255]));

    let transparent = RgbaImage::from_pixel(1, 1, Rgba([100, 120, 255, 0]));
    let cover = derive_cover(&png(&transparent)).unwrap();
    let image = decode_page(&cover.shelf_webp, MAX_OUTPUT_EDGE).unwrap();
    assert_eq!((image.width, image.height), (400, 600));
    assert!(image.pixels.iter().all(|&channel| channel == 255));
}

#[test]
fn invalid_cover_input_returns_a_fallback_signal() {
    assert!(matches!(
        derive_cover(b"broken image"),
        Err(MediaError::UnsupportedFormat)
    ));
}
