"""EPUB 导入器（契约 7.3）：container.xml → OPF → manifest/spine → XHTML 正文段落。

设计取舍（M1-PLAN §1）：
- 只取文本，不渲染内嵌样式/图片（封面/插图留 M2）；
- 按 spine 顺序一项 = 一章；段落用 HTMLParser 按块级标签切分（去 script/style，
  &nbsp; 归一，连续空行合并）；
- 标题优先 h1/h2/h3，其次 nav.xhtml / toc.ncx 对应项，缺失回退「第N部分」；
- 封面 / 书籍信息页等非正文 spine 项跳过（与 txt 书头清洗口径一致：
  本书 236 个 spine 项 → 234 章 / 15245 段，与同名 txt 完全对齐）。
"""
from __future__ import annotations

import io
import posixpath
import re
import urllib.parse
import zipfile
from html.parser import HTMLParser
from xml.etree import ElementTree as ET

from .importer import BookImportError, ChapterData, ParsedBook, title_author_from_filename

CONTAINER_PATH = "META-INF/container.xml"

_TEXT_MEDIA_TYPES = frozenset({
    "application/xhtml+xml",
    "text/html",
    "application/x-dtbook+xml",
})
_TEXT_EXTS = (".xhtml", ".html", ".htm")

_WS_RE = re.compile(r"\s+")
_META_FIRST_PARA_RE = re.compile(r"^(书籍信息|图书信息|版权信息|书名\s*[：:]|作者\s*[：:])")
_SKIP_TITLES = frozenset({"书籍信息", "图书信息", "版权信息"})
_SKIP_STEMS = frozenset({
    "cover", "coverpage", "cover-page", "info", "bookinfo", "book-info",
    "titlepage", "title-page", "toc", "nav", "contents", "copyright",
})

_BLOCK_TAGS = frozenset({
    "p", "div", "br", "h1", "h2", "h3", "h4", "h5", "h6", "li", "ul", "ol",
    "dl", "dt", "dd", "blockquote", "section", "article", "header", "footer",
    "main", "nav", "aside", "table", "tr", "td", "th", "hr", "pre", "figure",
    "figcaption", "body", "center", "address",
})
_HEADING_TAGS = frozenset({"h1", "h2", "h3"})
_SKIP_TAGS = frozenset({"script", "style", "noscript", "svg", "math", "template"})


# ---------------------------------------------------------------------------
# 路径 / 编码小工具
# ---------------------------------------------------------------------------

def _local(tag: str) -> str:
    """去掉 XML 命名空间，只留本地名（兼容 OPF/NCX 不同 ns 前缀）。"""
    return tag.rsplit("}", 1)[-1]


def _norm_zip_path(path: str) -> str:
    path = urllib.parse.unquote((path or "").replace("\\", "/").strip())
    while path.startswith("./"):
        path = path[2:]
    return path.lstrip("/")


def _resolve(base_dir: str, href: str) -> str:
    """把 OPF/NCX/NAV 中的相对 href 解析成 zip 内绝对路径（去 fragment）。"""
    href = (href or "").split("#", 1)[0]
    href = urllib.parse.unquote(href.replace("\\", "/").strip())
    if not href:
        return ""
    if href.startswith("/"):
        return _norm_zip_path(href)
    joined = posixpath.normpath(posixpath.join(base_dir, href)) if base_dir else posixpath.normpath(href)
    while joined.startswith("../"):
        joined = joined[3:]
    return joined.lstrip("/")


def _decode(data: bytes) -> str:
    """EPUB 规范要求 UTF-8/UTF-16；顺带容忍 gb18030 的野生 epub。"""
    for enc in ("utf-8-sig", "utf-8", "gb18030"):
        try:
            return data.decode(enc)
        except UnicodeDecodeError:
            continue
    return data.decode("utf-8", errors="replace")


def _clean_text(text: str) -> str:
    return _WS_RE.sub(" ", text.replace("\xa0", " ")).strip()


# ---------------------------------------------------------------------------
# zip 读取
# ---------------------------------------------------------------------------

class _ZipBook:
    """zip 访问封装：容忍大小写差异 / URL 编码 / 前导 ./ 的条目名。"""

    def __init__(self, zf: zipfile.ZipFile) -> None:
        self.zf = zf
        self.names = [n for n in zf.namelist() if not n.endswith("/")]
        self._names = frozenset(self.names)
        self._lower = {n.lower(): n for n in self.names}

    def read(self, path: str) -> bytes | None:
        key = _norm_zip_path(path)
        if not key:
            return None
        name = key if key in self._names else self._lower.get(key.lower())
        if name is None:
            return None
        try:
            return self.zf.read(name)
        except (KeyError, zipfile.BadZipFile, OSError):
            return None


def _find_opf(book: _ZipBook) -> str | None:
    raw = book.read(CONTAINER_PATH)
    if raw is not None:
        try:
            root = ET.fromstring(raw)
            for el in root.iter():
                if _local(el.tag) == "rootfile":
                    full_path = el.get("full-path")
                    if full_path:
                        return _norm_zip_path(full_path)
        except ET.ParseError:
            pass
    # 兜底：container.xml 缺失/损坏时，zip 内第一个 .opf
    for name in book.names:
        if name.lower().endswith(".opf"):
            return _norm_zip_path(name)
    return None


# ---------------------------------------------------------------------------
# OPF / NAV / NCX
# ---------------------------------------------------------------------------

def _parse_opf(book: _ZipBook, opf_path: str):
    raw = book.read(opf_path)
    if raw is None:
        raise BookImportError(
            "invalid_epub", "EPUB 中找不到 OPF 文件（container.xml 指向的文件不存在）", 400
        )
    try:
        root = ET.fromstring(raw)
    except ET.ParseError as exc:
        raise BookImportError("invalid_epub", f"EPUB OPF 解析失败：{exc}", 400) from exc

    opf_dir = posixpath.dirname(opf_path)
    title: str | None = None
    author: str | None = None
    items: dict[str, dict] = {}
    spine: list[str] = []

    for el in root.iter():
        name = _local(el.tag)
        if name == "title" and title is None:
            text = (el.text or "").strip()
            if text:
                title = text
        elif name == "creator" and author is None:
            text = (el.text or "").strip()
            if text:
                author = text
        elif name == "item":
            item_id = el.get("id")
            href = el.get("href")
            if not item_id or not href:
                continue
            items[item_id] = {
                "href": _resolve(opf_dir, href),
                "media-type": (el.get("media-type") or "").strip().lower(),
                "properties": (el.get("properties") or "").strip(),
            }
        elif name == "itemref":
            idref = el.get("idref")
            if idref:
                spine.append(idref)
    return title, author, items, spine


def _is_text_item(item: dict) -> bool:
    if (item.get("media-type") or "") in _TEXT_MEDIA_TYPES:
        return True
    return item.get("href", "").lower().endswith(_TEXT_EXTS)


class _AnchorCollector(HTMLParser):
    """收集 nav.xhtml 里 <a href> 文本：按文档顺序后写覆盖先写 → 叶子标题优先。"""

    def __init__(self, base_dir: str, titles: dict[str, str]) -> None:
        super().__init__(convert_charrefs=True)
        self.base_dir = base_dir
        self.titles = titles
        self._href: str | None = None
        self._buf: list[str] = []

    def handle_starttag(self, tag, attrs):
        if tag.lower() != "a" or self._href is not None:
            return
        href = dict(attrs).get("href")
        if href:
            self._href = href
            self._buf = []

    def handle_data(self, data):
        if self._href is not None:
            self._buf.append(data)

    def handle_endtag(self, tag):
        if tag.lower() != "a" or self._href is None:
            return
        title = _clean_text("".join(self._buf))
        target = _resolve(self.base_dir, self._href)
        if title and target:
            self.titles[target] = title
        self._href = None
        self._buf = []


def _ncx_titles(book: _ZipBook, ncx_item: dict) -> dict[str, str]:
    raw = book.read(ncx_item["href"])
    if raw is None:
        return {}
    base_dir = posixpath.dirname(ncx_item["href"])
    titles: dict[str, str] = {}
    try:
        root = ET.fromstring(raw)
    except ET.ParseError:
        return titles
    for el in root.iter():
        if _local(el.tag) != "navPoint":
            continue
        label: str | None = None
        src: str | None = None
        for child in el.iter():
            name = _local(child.tag)
            if name == "text" and label is None:
                label = (child.text or "").strip()
            elif name == "content" and src is None:
                src = child.get("src")
        if label and src:
            target = _resolve(base_dir, src)
            if target:
                titles[target] = label
    return titles


def _nav_titles(book: _ZipBook, items: dict[str, dict]) -> dict[str, str]:
    titles: dict[str, str] = {}
    nav_item = next(
        (it for it in items.values()
         if "nav" in (it.get("properties") or "").split() and _is_text_item(it)),
        None,
    )
    if nav_item is not None:
        raw = book.read(nav_item["href"])
        if raw is not None:
            try:
                parser = _AnchorCollector(posixpath.dirname(nav_item["href"]), titles)
                parser.feed(_decode(raw))
                parser.close()
            except Exception:  # nav 坏了不应拖垮整本导入
                pass
    ncx_item = next(
        (it for it in items.values()
         if it.get("media-type") == "application/x-dtbncx+xml"
         or it.get("href", "").lower().endswith(".ncx")),
        None,
    )
    if ncx_item is not None:
        titles.update(_ncx_titles(book, ncx_item))
    return titles


# ---------------------------------------------------------------------------
# XHTML 正文抽取
# ---------------------------------------------------------------------------

class _BodyParser(HTMLParser):
    """按块级标签切段：<p>/<div>/<br>/<h*>/<li>… 作为段落边界。

    只收 <body> 内文本；script/style/noscript/svg/math/template 整块跳过；
    首个 h1/h2/h3 段落记 kind 供标题识别（正文里后续标题仍作为普通段落保留）。
    """

    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.blocks: list[tuple[str, str]] = []
        self._buf: list[str] = []
        self._skip = 0
        self._in_body = False
        self._heading: str | None = None

    def _flush(self) -> None:
        text = _clean_text("".join(self._buf))
        self._buf = []
        kind = self._heading or "p"
        self._heading = None
        if text:
            self.blocks.append((kind, text))

    def handle_starttag(self, tag, attrs):
        name = tag.lower()
        if name in _SKIP_TAGS:
            self._skip += 1
            return
        if name == "body":
            self._in_body = True
            self._flush()
            return
        if not self._in_body:
            return
        if name in _BLOCK_TAGS:
            self._flush()
            if name in _HEADING_TAGS:
                self._heading = name

    def handle_startendtag(self, tag, attrs):
        # <br/>、<img/> 等自闭合标签：只起段落边界作用；自闭合的跳过标签无内容
        name = tag.lower()
        if name in _SKIP_TAGS:
            return
        if name in _BLOCK_TAGS:
            self._flush()
        else:
            self.handle_starttag(tag, attrs)

    def handle_endtag(self, tag):
        name = tag.lower()
        if name in _SKIP_TAGS:
            if self._skip:
                self._skip -= 1
            return
        if not self._in_body:
            return
        if name in _BLOCK_TAGS:
            self._flush()
        if name == "body":
            self._in_body = False

    def handle_data(self, data):
        if self._skip or not self._in_body:
            return
        self._buf.append(data)


def _chapter_from_xhtml(html_text: str, fallback_title: str | None, part_no: int) -> ChapterData:
    parser = _BodyParser()
    try:
        parser.feed(html_text)
        parser.close()
    except Exception:  # 野生 XHTML 容错
        pass
    blocks = parser.blocks
    title: str | None = None
    if blocks and blocks[0][0] in _HEADING_TAGS:
        title = blocks[0][1]
        blocks = blocks[1:]
    paragraphs = [text for _kind, text in blocks if text]
    if not title:
        title = (fallback_title or "").strip() or f"第{part_no}部分"
    return ChapterData(title=title, paras=paragraphs, chars=sum(len(p) for p in paragraphs))


def _is_metadata_or_empty(chapter: ChapterData, href: str) -> bool:
    if not chapter.paras:
        return True
    if chapter.title in _SKIP_TITLES:
        return True
    stem = posixpath.splitext(posixpath.basename(href))[0].strip().lower()
    if stem in _SKIP_STEMS:
        return True
    return bool(_META_FIRST_PARA_RE.match(chapter.paras[0]))


# ---------------------------------------------------------------------------
# 入口
# ---------------------------------------------------------------------------

def parse_book(raw: bytes, filename: str) -> ParsedBook:
    """解析 EPUB 字节流 → 与 txt 相同的章节结构（encoding 固定 utf-8）。"""
    try:
        zf = zipfile.ZipFile(io.BytesIO(raw))
    except (zipfile.BadZipFile, OSError) as exc:
        raise BookImportError("invalid_epub", f"不是有效的 EPUB（zip）文件：{exc}", 400) from exc

    with zf:
        book = _ZipBook(zf)
        opf_path = _find_opf(book)
        if not opf_path:
            raise BookImportError("invalid_epub", "EPUB 缺少 OPF 文件", 400)
        meta_title, meta_author, items, spine = _parse_opf(book, opf_path)
        nav_titles = _nav_titles(book, items)

        chapters: list[ChapterData] = []
        for idref in spine:
            item = items.get(idref)
            if not item or not _is_text_item(item):
                continue
            raw_xhtml = book.read(item["href"])
            if raw_xhtml is None:
                continue
            chapter = _chapter_from_xhtml(
                _decode(raw_xhtml), nav_titles.get(item["href"]), len(chapters) + 1
            )
            if _is_metadata_or_empty(chapter, item["href"]):
                continue
            chapters.append(chapter)

    if not chapters:
        raise BookImportError("empty_file", "EPUB 未解析出任何章节正文", 400)

    fallback_title, fallback_author = title_author_from_filename(filename)
    title = (meta_title or fallback_title or "未命名").strip() or "未命名"
    author = (meta_author or fallback_author or "").strip() or None
    return ParsedBook(
        title=title,
        author=author,
        encoding="utf-8",
        encoding_degraded=False,
        total_chars=sum(ch.chars for ch in chapters),
        paragraph_count=sum(len(ch.paras) for ch in chapters),
        chapters=chapters,
    )
