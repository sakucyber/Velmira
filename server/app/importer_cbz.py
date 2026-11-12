"""CBZ 漫画导入器（契约 7.3）。

流程：zip 内图片（jpg/jpeg/png/webp/gif）自然排序 → 解压到
DATA_DIR/media/<book_id>/<idx>.<ext> → Pillow 读宽高（失败填 0）→ 返回 book_pages 行。
不写 chapters；chapter_count = page_count。
"""
from __future__ import annotations

import io
import re
import shutil
import zipfile
from pathlib import Path

from . import config
from .importer import IMAGE_EXTS, BookImportError

try:  # Pillow 缺失时宽高退化为 0，不阻断导入
    from PIL import Image
except Exception:  # pragma: no cover
    Image = None  # type: ignore[assignment]

_NUM_RE = re.compile(r"(\d+)")


def natural_key(name: str) -> list:
    """数字感知排序键：1,2,10（而非 1,10,2）。"""
    return [int(part) if part.isdigit() else part.lower() for part in _NUM_RE.split(name)]


def list_image_names(names: list[str]) -> list[str]:
    """筛出图片条目并按自然排序（1,2,10）；跳过目录/macOS 元数据/隐藏文件。"""
    images: list[str] = []
    for name in names:
        if name.endswith("/") or name.startswith("__MACOSX/"):
            continue
        base = name.rsplit("/", 1)[-1]
        if not base or base.startswith("."):
            continue
        ext = base.rsplit(".", 1)[-1].lower() if "." in base else ""
        if ext in IMAGE_EXTS:
            images.append(name)
    images.sort(key=lambda name: (
        natural_key(name.rsplit("/", 1)[-1]),
        natural_key(name),
    ))
    return images


def _image_size(data: bytes) -> tuple[int, int]:
    if Image is None:
        return 0, 0
    try:
        with Image.open(io.BytesIO(data)) as img:
            width, height = img.size
        return int(width), int(height)
    except Exception:
        return 0, 0


def extract_pages(raw: bytes, book_id: str, reset: bool = False) -> list[dict]:
    """解压全部图片页并返回 [{idx,path,width,height,bytes}]。

    path 为相对 DATA_DIR 的 POSIX 路径（契约 7.2 book_pages.path）。
    reset=True（force 覆盖导入）时先清空旧 media 目录。
    """
    try:
        zf = zipfile.ZipFile(io.BytesIO(raw))
    except (zipfile.BadZipFile, OSError) as exc:
        raise BookImportError("invalid_cbz", f"不是有效的 CBZ（zip）文件：{exc}", 400) from exc

    with zf:
        names = list_image_names(zf.namelist())
        if not names:
            raise BookImportError(
                "empty_file", "CBZ 中没有图片（jpg/jpeg/png/webp/gif）", 400
            )

        media_dir = config.DATA_DIR / "media" / book_id
        if reset and media_dir.exists():
            shutil.rmtree(media_dir, ignore_errors=True)
        media_dir.mkdir(parents=True, exist_ok=True)

        pages: list[dict] = []
        try:
            for idx, name in enumerate(names):
                data = zf.read(name)
                ext = name.rsplit(".", 1)[-1].lower()
                filename = f"{idx}.{ext}"
                (media_dir / filename).write_bytes(data)
                width, height = _image_size(data)
                pages.append({
                    "idx": idx,
                    "path": f"media/{book_id}/{filename}",
                    "width": width,
                    "height": height,
                    "bytes": len(data),
                })
        except Exception:
            shutil.rmtree(media_dir, ignore_errors=True)
            raise
    return pages
