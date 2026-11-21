use std::io::{Cursor, Write};

use vm_format::{parse_epub, CbzReader, EpubReader, FormatError};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

fn zip_with(build: impl FnOnce(&mut ZipWriter<Cursor<Vec<u8>>>)) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    build(&mut zip);
    zip.finish().unwrap().into_inner()
}

fn archive(members: &[(&str, &[u8])]) -> Vec<u8> {
    zip_with(|zip| {
        let deflate = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for (name, contents) in members {
            zip.start_file(name, deflate).unwrap();
            zip.write_all(contents).unwrap();
        }
    })
}

// An actual single-pixel GIF as a synthetic comic page and EPUB resource.
const GIF: &[u8] = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff!\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;";

#[test]
fn cbz_sorts_naturally_and_reads_one_page_at_a_time() {
    let data = archive(&[
        ("001/10.GIF", GIF),
        ("001/2.gif", GIF),
        ("001/1.gif", GIF),
        ("001/.hidden.gif", GIF),
        ("__MACOSX/3.gif", GIF),
        ("ComicInfo.xml", b"<ComicInfo/>"),
    ]);
    let mut comic = CbzReader::new(Cursor::new(data), "Comic - Author.cbz").unwrap();
    assert_eq!(comic.title, "Comic");
    assert_eq!(comic.author.as_deref(), Some("Author"));
    assert_eq!(
        comic
            .pages()
            .iter()
            .map(|page| page.path.as_str())
            .collect::<Vec<_>>(),
        ["001/1.gif", "001/2.gif", "001/10.GIF"]
    );
    assert_eq!(comic.pages()[0].media_type, "image/gif");
    assert_eq!(comic.read_page(2).unwrap(), GIF);
    assert!(matches!(
        comic.read_page(3),
        Err(FormatError::PageOutOfRange(3))
    ));
}

#[test]
fn zip_rejects_traversal_symlinks_and_duplicate_paths() {
    let data = archive(&[("../unsafe.gif", GIF)]);
    assert!(matches!(
        CbzReader::new(Cursor::new(data), "a.cbz"),
        Err(FormatError::InvalidArchive(_))
    ));
    let data = zip_with(|zip| {
        zip.add_symlink("test.gif", "../outside", SimpleFileOptions::default())
            .unwrap();
    });
    assert!(matches!(
        CbzReader::new(Cursor::new(data), "a.cbz"),
        Err(FormatError::InvalidArchive(_))
    ));

    let mut data = archive(&[("a.gif", GIF), ("b.gif", GIF)]);
    for index in 0..data.len() - b"b.gif".len() {
        if &data[index..index + 5] == b"b.gif" {
            data[index] = b'a';
        }
    }
    assert!(matches!(
        CbzReader::new(Cursor::new(data), "a.cbz"),
        Err(FormatError::InvalidArchive(_))
    ));
}

#[test]
fn image_signature_is_checked_on_read() {
    let data = archive(&[("01.png", b"<svg>image</svg>")]);
    let mut cbz = CbzReader::new(Cursor::new(data), "comic.cbz").unwrap();
    assert!(matches!(
        cbz.read_page(0),
        Err(FormatError::InvalidArchive(_))
    ));
}

#[test]
fn epub_reads_spine_titles_cover_and_local_illustrations() {
    let container = br#"<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
        <rootfiles><rootfile full-path="OPS/book.opf"/></rootfiles></container>"#;
    let opf = r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/">
        <metadata><dc:title>夜读</dc:title><dc:creator>作者甲</dc:creator><meta name="cover" content="legacy" /></metadata>
        <manifest>
          <item id="first" href="Text/1.xhtml" media-type="application/xhtml+xml" />
          <item id="second" href="Text/2.xhtml" media-type="application/xhtml+xml" />
          <item id="front" href="Text/cover.xhtml" media-type="application/xhtml+xml" />
          <item id="legacy" href="Pictures/legacy.gif" media-type="image/gif" />
          <item id="modern" href="Pictures/modern.gif" media-type="image/gif" properties="cover-image" />
          <item id="nav" href="Text/nav.xhtml" media-type="application/xhtml+xml" properties="nav" />
          <item id="last" href="Text/3.xhtml" media-type="application/xhtml+xml" />
        </manifest>
        <spine><itemref idref="front"/><itemref idref="first"/><itemref idref="second"/><itemref idref="last"/></spine>
        <guide><reference type="cover" href="Text/cover.xhtml" /></guide>
    </package>"#;
    let cover = br#"<html><body><img src="../Pictures/front.gif"/></body></html>"#;
    let first = r#"<html><head><title>Head only</title></head><body><h1>第一章</h1>
        <p> A&nbsp;<em>quiet</em> evening. <img src="../Pictures/illustration%20one.gif" alt=" Illustration  caption "/> 后续。</p>
        <script>if (x < 2) { document.write("<p>hidden</p>") }</script>
        <style>.body{}</style><p>结束。</p><img src="https://example.invalid/remote.gif"/>
        <img src="../../../external.gif"/><img src="../Pictures/illustration%20one.gif"/>
        </body></html>"#;
    let second = "<html><body><p>第二项正文。</p></body></html>";
    let nav = "<html><body><p><a href='2.xhtml#text'>目录 &amp; 标题</a></p></body></html>";
    let last = "<html><body><img src='../Pictures/front.gif'></body></html>";
    let data = archive(&[
        ("META-INF/container.xml", container),
        ("OPS/book.opf", opf.as_bytes()),
        ("OPS/Text/cover.xhtml", cover),
        ("OPS/Text/1.xhtml", first.as_bytes()),
        ("OPS/Text/2.xhtml", second.as_bytes()),
        ("OPS/Text/nav.xhtml", nav.as_bytes()),
        ("OPS/Text/3.xhtml", last.as_bytes()),
        ("OPS/Pictures/front.gif", GIF),
        ("OPS/Pictures/legacy.gif", GIF),
        ("OPS/Pictures/modern.gif", GIF),
        ("OPS/Pictures/illustration one.gif", GIF),
    ]);

    let mut reader = EpubReader::new(Cursor::new(data)).unwrap();
    let book = reader.parse("fallback.epub").unwrap();
    assert_eq!(book.text.title, "夜读");
    assert_eq!(book.author.as_deref(), Some("作者甲"));
    assert_eq!(
        book.text
            .chapters
            .iter()
            .map(|chapter| chapter.title.as_str())
            .collect::<Vec<_>>(),
        ["第一章", "目录 & 标题", "第3部分"]
    );
    assert_eq!(
        book.text.chapters[0].paragraphs,
        ["A quiet evening. 后续。", "结束。"]
    );
    assert_eq!(book.text.chapters[1].paragraphs, ["第二项正文。"]);
    assert!(book.text.chapters[2].paragraphs.is_empty());
    assert_eq!(
        book.illustrations
            .iter()
            .map(|item| (item.chapter, item.before_para))
            .collect::<Vec<_>>(),
        [(0, 1), (0, 2), (2, 0)]
    );
    assert_eq!(book.illustrations[0].alt, "Illustration caption");
    assert_eq!(
        book.illustrations[0].image.path,
        "OPS/Pictures/illustration one.gif"
    );
    assert_eq!(
        reader.read_image(&book.illustrations[0].image).unwrap(),
        GIF
    );
    assert_eq!(
        book.cover_candidates
            .iter()
            .map(|image| image.path.as_str())
            .collect::<Vec<_>>(),
        [
            "OPS/Pictures/legacy.gif",
            "OPS/Pictures/modern.gif",
            "OPS/Pictures/front.gif"
        ]
    );
}

#[test]
fn epub_opf_does_not_process_dtd_entities() {
    let opf = br#"<!DOCTYPE package [<!ENTITY file SYSTEM "file:///local/private.txt">]>
        <package><metadata><title>&file;</title></metadata></package>"#;
    let data = archive(&[("book.opf", opf)]);
    assert!(matches!(
        parse_epub(&data, "book.epub"),
        Err(FormatError::EpubXml(_))
    ));
}
