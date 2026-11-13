"""SQLite 连接与初始化（契约第 3 节：DB_PATH / init_db / get_conn）。"""
from __future__ import annotations

import sqlite3
from pathlib import Path

from . import config

# 数据目录优先 DATA_DIR 环境变量，默认 项目根/data
DB_PATH: Path = config.DATA_DIR / "app.db"
_SCHEMA_PATH = Path(__file__).with_name("schema.sql")


def init_db() -> None:
    """幂等初始化：建目录 + 执行 schema.sql（WAL、表、索引、FTS5）。"""
    DB_PATH.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(DB_PATH, timeout=30)
    try:
        conn.executescript(_SCHEMA_PATH.read_text(encoding="utf-8"))
        # 只有首次升级需要去重；同一事务建立唯一索引，启动幂等且保留最早书签。
        conn.execute("BEGIN IMMEDIATE")
        if not conn.execute("SELECT 1 FROM sqlite_master WHERE name='idx_bookmarks_location'").fetchone():
            conn.execute("""
                DELETE FROM bookmarks WHERE id IN (
                    SELECT id FROM (
                        SELECT id, ROW_NUMBER() OVER (
                            PARTITION BY book_id, chapter, para ORDER BY created_at, id
                        ) AS rank FROM bookmarks
                    ) WHERE rank > 1
                )
            """)
            conn.execute("CREATE UNIQUE INDEX idx_bookmarks_location ON bookmarks(book_id, chapter, para)")
        conn.commit()
    finally:
        conn.close()


def get_conn() -> sqlite3.Connection:
    """新建连接：Row 工厂 + 外键 + 30s 忙等（WAL 并发写）。调用方负责 close。"""
    conn = sqlite3.connect(DB_PATH, timeout=30)
    conn.row_factory = sqlite3.Row
    conn.execute("PRAGMA foreign_keys=ON")
    return conn
