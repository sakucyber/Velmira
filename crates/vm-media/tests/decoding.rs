use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};
use vm_media::{decode_page, MediaError};

#[test]
fn jpeg_scales_and_preserves_aspect_and_colors() {
    let image = decode_page(include_bytes!("fixtures/page.jpg"), 100).unwrap();
    assert_eq!(
        (image.width, image.height, image.pixels.len()),
        (100, 50, 20_000)
    );
    assert!(image.pixels[0] > 210);
    assert!(image.pixels[2] < 40);
    let right = &image.pixels[99 * 4..][..4];
    assert!(right[0] < 40);
    assert!(right[2] > 190);
    assert_eq!(right[3], 255);
}

#[test]
fn jpeg_handles_exif_rotation_and_cmyk() {
    let rotated = decode_page(include_bytes!("fixtures/page-rotated.jpg"), 100).unwrap();
    assert_eq!((rotated.width, rotated.height), (50, 100));
    let cmyk = decode_page(include_bytes!("fixtures/page-cmyk.jpg"), 100).unwrap();
    assert_eq!((cmyk.width, cmyk.height), (16, 8));
    assert_eq!(&cmyk.pixels[..4], &[255, 0, 0, 255]);
}

#[test]
fn raster_formats_keep_alpha_and_small_images_are_not_enlarged() {
    let webp = decode_page(include_bytes!("fixtures/page.webp"), 64).unwrap();
    assert_eq!((webp.width, webp.height), (16, 8));
    assert_eq!(&webp.pixels[..4], &[21, 100, 192, 100]);

    let gif = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff!\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;";
    let gif = decode_page(gif, 64).unwrap();
    assert_eq!((gif.width, gif.height), (1, 1));
    assert_eq!(gif.pixels[3], 0);

    let rgba = [2, 3, 4, 128];
    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(&rgba, 1, 1, ExtendedColorType::Rgba8)
        .unwrap();
    let png = decode_page(&png, 64).unwrap();
    assert_eq!(png.pixels, rgba);
}

#[test]
fn rejects_oversized_and_unknown_images_from_header() {
    // The JPEG frame claims huge dimensions without allocating a huge test image.
    let mut big_jpeg = include_bytes!("fixtures/page.jpg").to_vec();
    let start = big_jpeg
        .windows(2)
        .position(|window| window == [0xff, 0xc0])
        .unwrap();
    big_jpeg[start + 5..start + 7].copy_from_slice(&8501u16.to_be_bytes());
    assert!(matches!(
        decode_page(&big_jpeg, 1600),
        Err(MediaError::Dimensions {
            width: 400,
            height: 8501
        })
    ));
    assert!(matches!(
        decode_page(b"not an image", 1600),
        Err(MediaError::UnsupportedFormat)
    ));
    assert!(matches!(
        decode_page(b"not an image", 1601),
        Err(MediaError::InvalidOutputEdge)
    ));
}
