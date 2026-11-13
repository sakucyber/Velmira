"""CBZ 漫画导入器（契约 7.3）。

流程：zip 内图片（jpg/jpeg/png/webp/gif）自然排序 → 解压到
DATA_DIR/media/<book_id>/<idx>.<ext> → Pillow 读宽高（失败填 0）→ 返回 book_pages 行。
不写 chapters；chapter_count = page_count。
"""
from __future__ import annotations

import io
import re
import tempfile
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


def _cbz_limit(message: str) -> BookImportError:
    return BookImportError("cbz_too_large", message, 413)


def _read_image_limited(zf: zipfile.ZipFile, info: zipfile.ZipInfo, total: int) -> bytes:
    """读取单页时再次按实际解压字节数限制，不能只信 zip 元数据。"""
    declared = int(info.file_size or 0)
    if declared > config.MAX_CBZ_PAGE_BYTES:
        raise _cbz_limit(
            f"CBZ 单页解压后超过 {config.MAX_CBZ_PAGE_BYTES // (1024 * 1024)}MB 上限"
        )
    if total + declared > config.MAX_CBZ_TOTAL_BYTES:
        raise _cbz_limit(
            f"CBZ 图片解压总量超过 {config.MAX_CBZ_TOTAL_BYTES // (1024 * 1024)}MB 上限"
        )

    data = bytearray()
    try:
        with zf.open(info, "r") as src:
            while True:
                chunk = src.read(min(1024 * 1024, config.MAX_CBZ_PAGE_BYTES + 1 - len(data)))
                if not chunk:
                    break
                data.extend(chunk)
                if len(data) > config.MAX_CBZ_PAGE_BYTES:
                    raise _cbz_limit(
                        f"CBZ 单页解压后超过 {config.MAX_CBZ_PAGE_BYTES // (1024 * 1024)}MB 上限"
                    )
                if total + len(data) > config.MAX_CBZ_TOTAL_BYTES:
                    raise _cbz_limit(
                        f"CBZ 图片解压总量超过 {config.MAX_CBZ_TOTAL_BYTES // (1024 * 1024)}MB 上限"
                    )
    except BookImportError:
        raise
    except (OSError, RuntimeError, zipfile.BadZipFile) as exc:
        raise BookImportError("invalid_cbz", f"CBZ 图片读取失败：{exc}", 400) from exc
    return bytes(data)


def extract_pages(raw: bytes, book_id: str, reset: bool = False) -> list[dict]:
    """解压全部图片页并返回 [{idx,path,width,height,bytes}]。

    path 为相对 DATA_DIR 的 POSIX 路径（契约 7.2 book_pages.path）。
    完整读取成功后再替换旧目录；失败不会破坏 force 导入前的可读页面。
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
        if len(names) > config.MAX_CBZ_PAGES:
            raise _cbz_limit(f"CBZ 图片页数超过 {config.MAX_CBZ_PAGES} 页上限")

        # 拒绝同名图片条目，确保每页只对应一个确定的 zip 条目。
        if len(set(names)) != len(names):
            raise BookImportError("invalid_cbz", "CBZ 包含重复的图片文件名", 400)
        info_by_name: dict[str, zipfile.ZipInfo] = {}
        for info in zf.infolist():
            info_by_name.setdefault(info.filename, info)
        image_infos: list[zipfile.ZipInfo] = []
        for name in names:
            info = info_by_name.get(name)
            if info is None:
                raise BookImportError("invalid_cbz", f"CBZ 图片条目不存在：{name}", 400)
            if int(info.file_size or 0) > config.MAX_CBZ_PAGE_BYTES:
                raise _cbz_limit(
                    f"CBZ 单页解压后超过 {config.MAX_CBZ_PAGE_BYTES // (1024 * 1024)}MB 上限"
                )
            image_infos.append(info)
        declared_total = sum(max(0, int(info.file_size or 0)) for info in image_infos)
        if declared_total > config.MAX_CBZ_TOTAL_BYTES:
            raise _cbz_limit(
                f"CBZ 图片解压总量超过 {config.MAX_CBZ_TOTAL_BYTES // (1024 * 1024)}MB 上限"
            )

        media_root = config.DATA_DIR / "media"
        media_root.mkdir(parents=True, exist_ok=True)
        media_dir = media_root / book_id
        pages: list[dict] = []
        # 临时目录与最终目录在同一卷；所有页验证完成后才替换旧目录。
        with tempfile.TemporaryDirectory(prefix=".cbz-", dir=media_root) as tmp:
            stage = Path(tmp) / "pages"
            stage.mkdir()
            total_bytes = 0
            for idx, (name, info) in enumerate(zip(names, image_infos)):
                data = _read_image_limited(zf, info, total_bytes)
                total_bytes += len(data)
                ext = name.rsplit(".", 1)[-1].lower()
                filename = f"{idx}.{ext}"
                (stage / filename).write_bytes(data)
                width, height = _image_size(data)
                pages.append({
                    "idx": idx,
                    "path": f"media/{book_id}/{filename}",
                    "width": width,
                    "height": height,
                    "bytes": len(data),
                })
            backup = Path(tmp) / "previous"
            if media_dir.exists():
                if not reset:
                    raise BookImportError("invalid_cbz", "漫画媒体目录已存在", 409)
                media_dir.rename(backup)
            try:
                stage.rename(media_dir)
            except Exception:
                if backup.exists():
                    backup.rename(media_dir)
                raise
    return pages
