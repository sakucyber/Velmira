use std::fs::{self, File};

use serde_json::{json, Value};
use vm_format::{parse_txt, parse_txt_detailed, read_txt, FormatError, MAX_TXT_BYTES};

// Generated from server.app.importer.parse_book at the v0.7.0 implementation.
#[test]
fn agrees_with_legacy_parser() {
    let cases: Vec<Value> = serde_json::from_str(include_str!("fixtures/txt-v0.7.0.json")).unwrap();
    for case in cases {
        let hex = case["raw_hex"].as_str().unwrap();
        let bytes = (0..hex.len())
            .step_by(2)
            .map(|pos| u8::from_str_radix(&hex[pos..pos + 2], 16).unwrap())
            .collect::<Vec<_>>();
        let actual = parse_txt_detailed(&bytes, case["file_name"].as_str().unwrap()).unwrap();
        let chapters = actual
            .text
            .chapters
            .iter()
            .map(|chapter| {
                json!({
                    "title": chapter.title,
                    "paras": chapter.paragraphs,
                    "chars": chapter.paragraphs.iter().map(|p| p.chars().count()).sum::<usize>(),
                })
            })
            .collect::<Vec<_>>();
        let actual = json!({
            "title": actual.text.title,
            "author": actual.author,
            "encoding": actual.legacy_encoding,
            "encoding_degraded": actual.encoding_degraded,
            "total_chars": actual.total_chars,
            "paragraph_count": actual.paragraph_count,
            "chapters": chapters,
        });
        assert_eq!(actual, case["expected"], "{}", case["name"]);
    }
}

#[test]
fn fallback_uses_scalar_offsets_and_prefers_newlines() {
    let input = format!("{}\n{}\n结尾", "甲".repeat(1900), "🙂".repeat(2600));
    let book = parse_txt(input.as_bytes(), "demo.txt").unwrap();
    assert_eq!(book.chapters.len(), 2);
    assert_eq!(book.chapters[0].title, "第1部分");
    assert_eq!(book.chapters[0].paragraphs, ["甲".repeat(1900)]);
    assert_eq!(
        book.chapters[1].paragraphs,
        ["🙂".repeat(2600), "结尾".to_owned()]
    );

    let trailing = format!("{}\n{}", "甲".repeat(1900), "乙".repeat(1099));
    let book = parse_txt(trailing.as_bytes(), "demo.txt").unwrap();
    assert_eq!(book.chapters.len(), 1);
    assert_eq!(book.chapters[0].paragraphs.len(), 2);
}

#[test]
fn empty_input_and_oversized_file_return_errors() {
    assert!(matches!(
        parse_txt("\u{feff}\u{200d}\r\n".as_bytes(), "demo.txt"),
        Err(FormatError::Empty)
    ));
    let path = std::env::temp_dir().join(format!("velmira-oversize-txt-{}", std::process::id()));
    let file = File::create(&path).unwrap();
    file.set_len(MAX_TXT_BYTES as u64 + 1).unwrap();
    drop(file);
    let result = read_txt(&path);
    fs::remove_file(path).unwrap();
    assert!(matches!(result, Err(FormatError::TooLarge)));
}
