"""导入流水线：格式分发（txt / epub / cbz）+ TXT 解析。

分发（契约 7.3）：bytes magic → zip 含 META-INF/container.xml 或 mimetype=application/epub+zip
→ EPUB；zip 且含图片 → CBZ；其它 → TXT。
TXT 流程：bytes → 编码探测 → 换行/零宽归一化 → 书头清洗 → 章节切分 → 段落索引 → SQLite 入库。
EPUB/CBZ 解析见 importer_epub.py / importer_cbz.py，入库/去重共用 import_book。

口径说明（Lead 2026-11-12 仲裁，契约 4.2 示例已同步）：
- 书头元信息 + 【简介】整块 + 章首【…】标签行 + 分隔线必须干净剥离，不污染第 1 章；
- total_chars = 归一化后全文长度（含书头块），本书 516,256；
- paragraphs/para_count = 入库段落合计（干净口径），本书 15,245。
"""
from __future__ import annotations

import hashlib
import io
import json
import re
import secrets
import threading
import time
import zipfile
from dataclasses import dataclass
from pathlib import Path
from time import perf_counter

from . import config
from .db import get_conn

# 零宽 / 方向控制 / BOM 字符
ZERO_WIDTH_RE = re.compile("[\u200b-\u200f\u2060\ufeff]")

# 书头元信息行（命中即从章节正文中丢弃）：书名/原名/作者/字数/书籍 ID/...
META_RE = re.compile(
    r"^\s*(书籍信息|书名|原名|作者|连载状态|字数|章节数|书籍\s*ID|阅读量|最后更新时间|【简介】)"
    r"\s*[：:]?\s*(.*)$"
)

# 章首标签行（如【类型标签】）与分隔线（---/===/***/——）
TAG_LINE_RE = re.compile(r"^【[^】]*】$")
SEP_LINE_RE = re.compile(r"^[-=*—_]{3,}$")

# 章节标题 4 条正则（ARCHITECTURE 5.2）
# 1) 第N章/回/节/卷：阿拉伯数字后允许紧跟标题；中文数字后要求空白/标点，
#    避免「第一节课就被…」这类正文被误判成标题。
_TITLE_PATTERNS = (
    re.compile(
        r"^(?:"
        r"第\s*[0-9]+\s*[章回卷]"
        r"|第\s*[0-9]+\s*[节](?=[\s：:、，,．.\-—]|$)"
        r"|第\s*[零一二三四五六七八九十百千万两]+\s*[章回卷节](?=[\s：:、，,．.\-—]|$)"
        r")"
    ),
    re.compile(r"^Chapter\s+\d+\b.*$", re.IGNORECASE),
    re.compile(r"^(?:楔子|序章|序言|引子|前言|尾声|后记|番外(?:篇)?)(?:\s*\S{1,24})?$"),
    re.compile(r"^\d{1,4}\s*[、.．]\s*\S{1,30}$"),
)

CHAPTER_TITLE_MAX_LEN = 40
FALLBACK_CHUNK_CHARS = 3000

# M1：zip 内图片扩展名（cbz 判定与 importer_cbz 共用）
IMAGE_EXTS = frozenset({"jpg", "jpeg", "png", "webp", "gif"})

# M1：原始文件落盘后缀（按检测到的格式，便于排查/重放）
_STORED_SUFFIX = {"txt": ".txt", "epub": ".epub", "cbz": ".cbz"}
_IMPORT_LOCK = threading.Lock()


class BookImportError(Exception):
    """导入失败：code/status_code 直接映射到契约错误体。"""

    def __init__(self, code: str, message: str, status_code: int = 400) -> None:
        super().__init__(message)
        self.code = code
        self.message = message
        self.status_code = status_code


@dataclass
class ChapterData:
    title: str
    paras: list[str]
    chars: int


@dataclass
class ParsedBook:
    title: str
    author: str | None
    encoding: str
    encoding_degraded: bool
    total_chars: int
    paragraph_count: int
    chapters: list[ChapterData]


# ---------------------------------------------------------------------------
# 编码探测 / 归一化
# ---------------------------------------------------------------------------

def decode_bytes(raw: bytes) -> tuple[str, str, bool]:
    """按 BOM → utf-8 严格 → gb18030 → utf-8(replace) 探测编码。

    返回 (text, encoding, degraded)。encoding 报真实探测结果：
    - 有 UTF-8 BOM → utf-8-sig；有 UTF-16 BOM → utf-16-le / utf-16-be
    - 无 BOM 但 utf-8 严格解码成功 → utf-8（本书即此情况，契约 4.2 要求不得伪报 utf-8-sig）
    - gb18030 成功 → gb18030；最后 errors=replace 兜底 → utf-8 且 degraded=True
    """
    if raw.startswith(b"\xef\xbb\xbf"):
        try:
            return raw.decode("utf-8-sig"), "utf-8-sig", False
        except UnicodeDecodeError:
            pass
    if raw.startswith(b"\xff\xfe"):
        try:
            return raw.decode("utf-16-le"), "utf-16-le", False
        except UnicodeDecodeError:
            pass
    if raw.startswith(b"\xfe\xff"):
        try:
            return raw.decode("utf-16-be"), "utf-16-be", False
        except UnicodeDecodeError:
            pass
    try:
        return raw.decode("utf-8"), "utf-8", False
    except UnicodeDecodeError:
        pass
    try:
        return raw.decode("gb18030"), "gb18030", False
    except UnicodeDecodeError:
        pass
    # 最后兜底：不抛错，标记降级（调用方仍可继续导入，只是有替换字符）
    return raw.decode("utf-8", errors="replace"), "utf-8", True


def normalize_text(text: str) -> str:
    """去 BOM/零宽字符 + 统一换行（\\r\\n、\\r → \\n）。"""
    if text.startswith("\ufeff"):
        text = text[1:]
    text = text.replace("\r\n", "\n").replace("\r", "\n")
    return ZERO_WIDTH_RE.sub("", text)


# ---------------------------------------------------------------------------
# 书头 / 标题 / 章节
# ---------------------------------------------------------------------------

def _extract_meta(lines: list[str]) -> dict[str, str]:
    """读取书头元信息（只扫前 40 行）。"""
    meta: dict[str, str] = {}
    for line in lines[:40]:
        m = META_RE.match(line)
        if not m:
            continue
        key = m.group(1).replace(" ", "")
        val = m.group(2).strip()
        if val and key not in meta:
            meta[key] = val
    return meta


def title_author_from_filename(filename: str) -> tuple[str, str | None]:
    """兜底标题：文件名去扩展名；「 - 作者」后缀拆出作者。"""
    name = Path((filename or "").replace("\\", "/")).name
    stem = Path(name).stem or "未命名"
    title, author = stem, None
    if " - " in stem:
        left, _, right = stem.partition(" - ")
        if left.strip():
            title = left.strip()
        if right.strip():
            author = right.strip()
    return title, author


# 兼容旧名（M0 内部引用）
_title_author_from_filename = title_author_from_filename


def _is_title_line(line: str) -> bool:
    if not line or len(line) > CHAPTER_TITLE_MAX_LEN:
        return False
    return any(p.match(line) for p in _TITLE_PATTERNS)


def _find_title_indices(lines: list[str]) -> list[int]:
    return [i for i, line in enumerate(lines) if _is_title_line(line)]


def _is_chaptered(lines: list[str], starts: list[int]) -> bool:
    """前 3 个候选标题里至少命中 2 个强章节模式，才认定是章节体。"""
    if len(starts) < 2:
        return False
    strong = sum(1 for i in starts[:3] if _TITLE_PATTERNS[0].match(lines[i])
                 or _TITLE_PATTERNS[1].match(lines[i])
                 or _TITLE_PATTERNS[2].match(lines[i]))
    return strong >= 2


def _split_chapters(lines: list[str], starts: list[int]) -> list[ChapterData]:
    """按标题切章；标题之前的书头块整块丢弃。"""
    chapters: list[ChapterData] = []
    for k, start in enumerate(starts):
        end = starts[k + 1] if k + 1 < len(starts) else len(lines)
        paras = [line.strip() for line in lines[start + 1:end] if line.strip()]
        # 章首的标签行 / 分隔线逐条剥掉
        while paras and (TAG_LINE_RE.match(paras[0]) or SEP_LINE_RE.match(paras[0])):
            paras.pop(0)
        chapters.append(ChapterData(title=lines[start].strip(), paras=paras,
                                    chars=sum(len(p) for p in paras)))
    return chapters


def _split_fixed(text: str) -> list[ChapterData]:
    """无章节体兜底：每 3000 字一份，尽量切在换行处，标题「第N部分」。"""
    chapters: list[ChapterData] = []
    pos, total, part = 0, len(text), 0
    while pos < total:
        end = min(pos + FALLBACK_CHUNK_CHARS, total)
        if end < total:
            cut = text.rfind("\n", pos, end)
            if cut > pos + FALLBACK_CHUNK_CHARS // 2:
                end = cut
        chunk = text[pos:end]
        pos = end
        paras = [line.strip() for line in chunk.split("\n") if line.strip()]
        while paras and (TAG_LINE_RE.match(paras[0]) or SEP_LINE_RE.match(paras[0])):
            paras.pop(0)
        if paras:
            part += 1
            chapters.append(ChapterData(title=f"第{part}部分", paras=paras,
                                        chars=sum(len(p) for p in paras)))
    return chapters


# ---------------------------------------------------------------------------
# 解析入口
# ---------------------------------------------------------------------------

def parse_book(raw: bytes, filename: str) -> ParsedBook:
    text, encoding, degraded = decode_bytes(raw)
    text = normalize_text(text)
    if not text.strip():
        raise BookImportError("empty_file", "文件内容为空（解码后无有效文本）", 400)

    lines = [line.strip() for line in text.split("\n")]
    meta = _extract_meta(lines)
    starts = _find_title_indices(lines)

    if _is_chaptered(lines, starts):
        chapters = _split_chapters(lines, starts)
    else:
        chapters = _split_fixed(text)
    # 段数 = 实际入库段落合计（书头已剥离）
    paragraph_count = sum(len(ch.paras) for ch in chapters)

    fb_title, fb_author = _title_author_from_filename(filename)
    title = (meta.get("书名") or fb_title or "未命名").strip()
    author = (meta.get("作者") or fb_author or "").strip() or None

    return ParsedBook(
        title=title,
        author=author,
        encoding=encoding,
        encoding_degraded=degraded,
        total_chars=len(text),
        paragraph_count=paragraph_count,
        chapters=chapters,
    )


# ---------------------------------------------------------------------------
# 入库 / 查询辅助
# ---------------------------------------------------------------------------

_PARA_COUNTS: dict[str, int] = {}


def book_to_dict(row) -> dict:
    """行 → 契约 4.2 的 book 对象；M1：cbz 追加 page_count（契约 7.3）。"""
    data = {
        "id": row["id"],
        "title": row["title"],
        "author": row["author"],
        "format": row["format"],
        "encoding": row["encoding"],
        "total_chars": row["total_chars"],
        "chapter_count": row["chapter_count"],
        "created_at": row["created_at"],
    }
    if (row["format"] or "txt") == "cbz":
        # 契约 7.3：cbz 的 chapter_count = page_count
        data["page_count"] = int(row["chapter_count"] or 0)
    return data


def paragraph_count_for(row) -> int:
    """书籍段数：内存缓存优先，重启后从 chapters 汇总（干净口径，与导入返回一致）。"""
    book_id = row["id"]
    cached = _PARA_COUNTS.get(book_id)
    if cached is not None:
        return cached
    count = 0
    conn = get_conn()
    try:
        count = sum(len(json.loads(r["paras_json"]))
                    for r in conn.execute("SELECT paras_json FROM chapters WHERE book_id=?", (book_id,)))
    finally:
        conn.close()
    _PARA_COUNTS[book_id] = count
    return count


# ---------------------------------------------------------------------------
# M1：格式自动识别与分发（契约 7.3，以字节为准，扩展名仅作提示）
# ---------------------------------------------------------------------------

def detect_format(raw: bytes) -> str:
    """按 magic 判断 txt / epub / cbz。

    - zip 且含 META-INF/container.xml 或 mimetype=application/epub+zip → epub
    - zip 且非 epub，内含 >=1 张图片（jpg/jpeg/png/webp/gif） → cbz
    - 其它 → txt（沿用 v0.1 编码探测）
    """
    if not raw or raw[:4] not in (b"PK\x03\x04", b"PK\x05\x06", b"PK\x07\x08"):
        return "txt"
    try:
        zf = zipfile.ZipFile(io.BytesIO(raw))
    except (zipfile.BadZipFile, OSError):
        return "txt"
    try:
        names = zf.namelist()
        lower_names = [n.lower() for n in names]
        if "meta-inf/container.xml" in lower_names:
            return "epub"
        # 少数 epub 缺 container.xml，但 mimetype 仍为 application/epub+zip
        for name, lower in zip(names, lower_names):
            if lower == "mimetype":
                try:
                    with zf.open(name) as mimetype:
                        if mimetype.read(128).strip() == b"application/epub+zip":
                            return "epub"
                except (KeyError, zipfile.BadZipFile, OSError):
                    pass
                break
        for name in names:
            if name.endswith("/") or name.startswith("__MACOSX/"):
                continue
            base = name.rsplit("/", 1)[-1]
            ext = base.rsplit(".", 1)[-1].lower() if "." in base else ""
            if ext in IMAGE_EXTS:
                return "cbz"
        return "txt"
    finally:
        zf.close()


def _store_source(raw: bytes, book_id: str, fmt: str) -> Path:
    """保留原文件供书籍详情下载备份/重新导入，路径写入 books.file_path。"""
    books_dir = config.DATA_DIR / "books"
    books_dir.mkdir(parents=True, exist_ok=True)
    stored_path = books_dir / f"{book_id}{_STORED_SUFFIX.get(fmt, '.bin')}"
    stored_path.write_bytes(raw)
    return stored_path


def _import_txt(raw: bytes, filename: str) -> dict:
    parsed = parse_book(raw, filename)
    return {
        "format": "txt",
        "title": parsed.title,
        "author": parsed.author,
        "encoding": parsed.encoding,
        "total_chars": parsed.total_chars,
        "chapters": parsed.chapters,
        "pages": [],
        "paragraph_count": parsed.paragraph_count,
        "chapter_count": len(parsed.chapters),
    }


def _import_epub(raw: bytes, filename: str) -> dict:
    from . import importer_epub

    parsed, assets, formats, occurrences = importer_epub.parse_book(raw, filename, return_assets=True)
    return {
        "format": "epub",
        "title": parsed.title,
        "author": parsed.author,
        "encoding": parsed.encoding,
        "total_chars": parsed.total_chars,
        "chapters": parsed.chapters,
        "pages": [],
        "paragraph_count": parsed.paragraph_count,
        "chapter_count": len(parsed.chapters),
        "assets": assets,
        "formats": formats,
        "occurrences": occurrences,
    }


def _import_cbz(raw: bytes, book_id: str, filename: str, reset: bool) -> dict:
    from . import importer_cbz

    pages = importer_cbz.extract_pages(raw, book_id, reset=reset)
    title, author = title_author_from_filename(filename)
    return {
        "format": "cbz",
        "title": (title or "未命名").strip(),
        "author": (author or "").strip() or None,
        "encoding": None,
        "total_chars": 0,
        "chapters": [],
        "assets": [],
        "formats": [],
        "pages": pages,
        "paragraph_count": 0,
        "chapter_count": len(pages),
    }


def import_book(raw: bytes, filename: str, force: bool = False) -> dict:
    # 单进程部署：线程池可并发导入，同一内容的查重、文件写入和 DB 提交
    # 必须串行，避免两个请求同时创建相同 SHA 的书籍和孤立媒体目录。
    with _IMPORT_LOCK:
        return _import_book(raw, filename, force)


def _import_book(raw: bytes, filename: str, force: bool = False) -> dict:
    """完整导入（去重/覆盖/格式分发）：返回契约 4.2 / 7.3 的响应体。"""
    t0 = perf_counter()
    if not raw:
        raise BookImportError("empty_file", "上传文件为空", 400)
    if len(raw) > config.MAX_UPLOAD_BYTES:
        raise BookImportError("file_too_large", "文件超过 64MB 上限", 413)

    sha = hashlib.sha256(raw).hexdigest()
    conn = get_conn()
    try:
        existing = conn.execute("SELECT * FROM books WHERE sha256=?", (sha,)).fetchone()
        if existing is not None and not force:
            return {
                "book": book_to_dict(existing),
                "import_ms": int((perf_counter() - t0) * 1000),
                "paragraphs": paragraph_count_for(existing),
                "deduped": True,
            }

        fmt = detect_format(raw)
        book_id = existing["id"] if existing is not None else "b_" + secrets.token_hex(3)

        if fmt == "cbz":
            data = _import_cbz(raw, book_id, filename, reset=existing is not None)
        elif fmt == "epub":
            data = _import_epub(raw, filename)
        else:
            data = _import_txt(raw, filename)

        stored_path = _store_source(raw, book_id, fmt)
        now = int(time.time())
        created_at = existing["created_at"] if existing is not None else now
        chapter_rows = [
            (f"{book_id}-c{idx}", book_id, idx, ch.title, ch.chars,
             json.dumps(ch.paras, ensure_ascii=False))
            for idx, ch in enumerate(data["chapters"])
        ]
        page_rows = [
            (book_id, p["idx"], p["path"], p["width"], p["height"], p["bytes"])
            for p in data["pages"]
        ]
        asset_rows = [
            (f"{book_id}-asset-{idx}", book_id, chapter_idx, path, media, blob, len(blob))
            for idx, (chapter_idx, path, media, blob) in enumerate(data.get("assets", []))
        ]
        format_rows = []
        for chapter_idx, blocks in enumerate(data.get("formats", [])):
            for para_idx, spans in enumerate(blocks):
                if spans:
                    format_rows.append((book_id, chapter_idx, para_idx, json.dumps(spans, ensure_ascii=False)))

        # 单事务写入（force 时先清掉旧章节与派生产物）
        with conn:
            if existing is None:
                conn.execute(
                    "INSERT INTO books(id,title,author,format,file_path,sha256,encoding,"
                    "total_chars,chapter_count,created_at) VALUES(?,?,?,?,?,?,?,?,?,?)",
                    (book_id, data["title"], data["author"], fmt, str(stored_path), sha,
                     data["encoding"], data["total_chars"], data["chapter_count"], created_at),
                )
            else:
                for table in ("chapters", "segments", "para_fts", "search_index_state",
                    "book_pages", "epub_assets", "epub_paragraph_formats", "epub_illustrations"):
                    conn.execute(f"DELETE FROM {table} WHERE book_id=?", (book_id,))
                conn.execute(
                    "UPDATE books SET title=?,author=?,format=?,file_path=?,sha256=?,encoding=?,"
                    "total_chars=?,chapter_count=? WHERE id=?",
                    (data["title"], data["author"], fmt, str(stored_path), sha,
                     data["encoding"], data["total_chars"], data["chapter_count"], book_id),
                )
            if chapter_rows:
                conn.executemany(
                    "INSERT INTO chapters(id,book_id,idx,title,chars,paras_json)"
                    " VALUES(?,?,?,?,?,?)",
                    chapter_rows,
                )
            if page_rows:
                conn.executemany(
                    "INSERT INTO book_pages(book_id,idx,path,width,height,bytes)"
                    " VALUES(?,?,?,?,?,?)",
                    page_rows,
                )
            if asset_rows:
                conn.executemany(
                    "INSERT INTO epub_assets(id,book_id,chapter_idx,path,media_type,image_bytes,bytes) VALUES(?,?,?,?,?,?,?)",
                    asset_rows,
                )
            if data.get("occurrences"):
                asset_ids = {}
                for row in asset_rows:
                    asset_ids.setdefault(row[3], row[0])
                illustration_rows = []
                ordinals = {}
                for chapter_idx, before_para, path, alt in data["occurrences"]:
                    asset_id = asset_ids.get(path)
                    if not asset_id:
                        continue
                    ordinal = ordinals.get(chapter_idx, 0); ordinals[chapter_idx] = ordinal + 1
                    illustration_rows.append((book_id, chapter_idx, ordinal, before_para, asset_id, alt or ""))
                if illustration_rows:
                    conn.executemany(
                        "INSERT INTO epub_illustrations(book_id,chapter_idx,ordinal,before_para,asset_id,alt) VALUES(?,?,?,?,?,?)",
                        illustration_rows,
                    )
            if format_rows:
                conn.executemany(
                    "INSERT INTO epub_paragraph_formats(book_id,chapter_idx,para_idx,formats_json) VALUES(?,?,?,?)",
                    format_rows,
                )

        _PARA_COUNTS[book_id] = int(data["paragraph_count"])
        row = conn.execute("SELECT * FROM books WHERE id=?", (book_id,)).fetchone()
        return {
            "book": book_to_dict(row),
            "import_ms": int((perf_counter() - t0) * 1000),
            "paragraphs": int(data["paragraph_count"]),
            "deduped": False,
        }
    finally:
        conn.close()
