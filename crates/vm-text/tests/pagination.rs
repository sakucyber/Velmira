use vm_text::{LayoutError, Loc, PageBox, Paginator};

const FONT: &[u8] = include_bytes!("fixtures/NotoSansSC-test.ttf");
const TEXT: &str = "中文阅读小说，春风山水。明月长夜，书香江湖！小说阅读，中文。";

fn paginator() -> Paginator {
    Paginator::from_font_data(FONT.to_vec())
}

fn page(width: f32, height: f32) -> PageBox {
    PageBox {
        width,
        height,
        font_size: 19.0,
        line_height: 28.0,
    }
}

#[test]
fn chinese_pages_remap_by_character_after_resize() {
    let mut paginator = paginator();
    let text = TEXT.repeat(3);
    let compact = paginator.paginate(4, &[&text], page(100.0, 57.0)).unwrap();
    let expanded = paginator.paginate(4, &[&text], page(230.0, 112.0)).unwrap();
    assert_eq!(compact.missing_glyphs(), 0);
    assert_eq!(expanded.missing_glyphs(), 0);
    assert!(compact.page_count() > expanded.page_count());
    let saved = compact.page_start(2).unwrap();
    assert_eq!(saved.chapter, 4);
    assert_eq!(saved.para, 0);
    assert!(saved.off > 0 && saved.off < text.chars().count() as u32);
    assert_eq!(compact.page_for(saved), Some(2));
    let expanded_index = expanded.page_for(saved).unwrap();
    assert!(expanded.page_start(expanded_index).unwrap() <= saved);
    if let Some(next) = expanded.page_start(expanded_index + 1) {
        assert!(next > saved);
    }
    assert_eq!(expanded.page_for(Loc { off: 9999, ..saved }), None);
    assert_eq!(
        expanded.page_for(Loc {
            chapter: 3,
            ..saved
        }),
        None
    );
}

#[test]
fn wraps_chinese_closing_punctuation_with_preceding_character() {
    let result = paginator()
        .paginate(0, &["明月，春风。中文。"], page(40.0, 28.0))
        .unwrap();
    let text: Vec<_> = "明月，春风。中文。".chars().collect();
    assert!(result.page_count() >= 3);
    for i in 1..result.page_count() {
        let start = result.page_start(i).unwrap();
        assert!(
            !"。，！？、）】".contains(text[start.off as usize]),
            "{start:?}"
        );
    }
}

#[test]
fn maps_glyph_edges_and_highlights_to_unicode_positions() {
    let result = paginator()
        .paginate(0, &["中文office", "阅读"], page(250.0, 90.0))
        .unwrap();
    assert_eq!(result.hit_test(0, -10.0, 12.0), Some(Loc::default()));
    assert_eq!(
        result.hit_test(0, 400.0, 12.0),
        Some(Loc {
            chapter: 0,
            para: 0,
            off: 8
        })
    );
    assert_eq!(
        result.hit_test(0, -10.0, 40.0),
        Some(Loc {
            chapter: 0,
            para: 1,
            off: 0
        })
    );
    let end = Loc {
        chapter: 0,
        para: 0,
        off: 2,
    };
    let rects = result.range_rects(0, (Loc::default(), end));
    assert_eq!(rects.len(), 1);
    assert!((36.0..=40.0).contains(&rects[0].width));
    assert_eq!(rects[0].height, 28.0);
    assert_eq!(result.hit_test(0, f32::NAN, 0.0), None);
}

#[test]
fn preserves_explicit_line_break_offsets() {
    let result = paginator()
        .paginate(0, &["中文\r\n阅读"], page(70.0, 28.0))
        .unwrap();
    assert_eq!(result.page_count(), 2);
    assert_eq!(
        result.page_start(1),
        Some(Loc {
            chapter: 0,
            para: 0,
            off: 4
        })
    );
}

#[test]
fn rasterizes_only_requested_page_as_transparent_rgba() {
    let mut paginator = paginator();
    let layout = paginator.paginate(1, &[TEXT], page(100.0, 28.0)).unwrap();
    let rgba = [33, 52, 77, 255];
    let first = paginator.rasterize_page(&layout, 0, rgba).unwrap();
    let second = paginator.rasterize_page(&layout, 1, rgba).unwrap();
    assert_eq!(first.rgba.len(), 100 * 28 * 4);
    assert_eq!((first.width, first.height), (100, 28));
    assert_ne!(first.rgba, second.rgba);
    let (pixels, remainder) = first.rgba.as_chunks::<4>();
    assert!(remainder.is_empty());
    assert!(pixels.iter().any(|pixel| pixel[3] > 0));
    assert!(pixels.iter().any(|pixel| pixel[3] == 0));
    assert!(pixels
        .iter()
        .filter(|pixel| pixel[3] != 0)
        .all(|pixel| pixel[..3] == rgba[..3]));
    assert_eq!(
        paginator
            .rasterize_page(&layout, layout.page_count(), rgba)
            .unwrap_err(),
        LayoutError::NoSuchPage
    );
    assert!(paginator
        .rasterize_page(&layout, 0, [255, 255, 255, 0])
        .unwrap()
        .rgba
        .iter()
        .all(|&x| x == 0));
}

#[test]
fn rejects_invalid_dimensions_text_budget_and_missing_fonts() {
    let mut paginator = paginator();
    assert_eq!(
        paginator
            .paginate(0, &[TEXT], page(f32::NAN, 57.0))
            .unwrap_err(),
        LayoutError::InvalidPageBox
    );
    assert_eq!(
        paginator
            .paginate(0, &["春".repeat(400_000)], page(100.0, 57.0))
            .unwrap_err(),
        LayoutError::ChapterTooLarge
    );
    assert_eq!(
        Paginator::from_font_data(Vec::new())
            .paginate(0, &[TEXT], page(100.0, 57.0))
            .unwrap_err(),
        LayoutError::NoFonts
    );
}
