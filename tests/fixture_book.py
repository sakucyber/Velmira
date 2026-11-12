#!/usr/bin/env python
"""本地测试夹具探测（tests/ 专用，verifier 所有）。

脱敏约定：tests/ 下不硬编码具体小说内容（书名/作者/角色名/原文句子）。
所有与夹具内容相关的值都在运行时从 book/ 目录的第一本 txt（及同名 epub）解析：
- 书名/作者：优先解析文件头「书籍信息 / 书名： / 作者：」，回退文件名「标题 - 作者」
- 搜索词：由调用方从接口返回的正文里截取 2/3 字，本模块提供 pick_term()

本地夹具本身（book/*.txt、book/*.epub）不属于 tests/ 证据，保留原文。
"""
from __future__ import annotations

import glob
import os
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BOOK_DIR = ROOT / "book"

_CJK_RUN = re.compile(r"[\u4e00-\u9fff]{2,}")


def find_txt() -> Path:
    """book/ 下按文件名排序的第一本 .txt。"""
    files = sorted(glob.glob(str(BOOK_DIR / "*.txt")))
    if not files:
        raise FileNotFoundError("book/ 下没有 .txt 夹具")
    return Path(files[0])


def find_epub(txt: Path | None = None) -> Path | None:
    """与 txt 同名的 .epub；不存在则返回 book/ 下第一本 .epub。"""
    txt = txt or find_txt()
    same = txt.with_suffix(".epub")
    if same.is_file():
        return same
    files = sorted(glob.glob(str(BOOK_DIR / "*.epub")))
    return Path(files[0]) if files else None


def _head_lines(path: Path, limit: int = 60) -> list[str]:
    lines: list[str] = []
    with open(path, "r", encoding="utf-8", errors="replace") as fh:
        for _ in range(limit):
            line = fh.readline()
            if not line:
                break
            lines.append(line.rstrip("\r\n"))
    return lines


def parse_meta(path: Path | None = None) -> dict:
    """解析夹具标题/作者。返回 {title, author, txt, epub, filename}。"""
    txt = path or find_txt()
    stem = txt.stem
    title = ""
    author = ""
    lines = _head_lines(txt)
    for i, line in enumerate(lines):
        s = line.strip()
        for prefix in ("书名：", "书名:", "标题：", "标题:"):
            if s.startswith(prefix) and len(s) > len(prefix):
                title = s[len(prefix):].strip()
        for prefix in ("作者：", "作者:"):
            if s.startswith(prefix):
                author = s[len(prefix):].strip()
        if not title and s == "书籍信息":
            for nxt in lines[i + 1:i + 8]:
                cand = nxt.strip()
                if cand and not cand.startswith(("原名", "作者", "书籍状态", "字数", "章节数", "书籍 ID")):
                    title = cand
                    break
    if not title:
        # 回退文件名：「标题 - 作者」
        parts = [p.strip() for p in stem.split(" - ")]
        title = parts[0]
        if not author and len(parts) >= 2:
            author = parts[-1]
    if not author:
        parts = [p.strip() for p in stem.split(" - ")]
        if len(parts) >= 2:
            author = parts[-1]
    return {"title": title, "author": author, "txt": str(txt),
            "epub": str(find_epub(txt)) if find_epub(txt) else None,
            "filename": txt.name}


def title() -> str:
    return parse_meta()["title"]


def author() -> str:
    return parse_meta()["author"]


def pick_term(text: str, n: int = 2) -> str:
    """从正文动态截取 n 字搜索词（优先连续汉字），不硬编码内容。"""
    text = text or ""
    for m in _CJK_RUN.finditer(text):
        run = m.group(0)
        if len(run) >= n:
            return run[:n]
    stripped = text.strip()
    return stripped[:n] if stripped else ""


def placeholder(text: str | None = None) -> str:
    """统一占位：证据/JSON 中不打印原文。"""
    return "（本地夹具片段）"
