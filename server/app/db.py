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
        memory_fact_columns = {row[1] for row in conn.execute("PRAGMA table_info(chapter_memory_facts)")}
        if "kind" not in memory_fact_columns:
            conn.execute("ALTER TABLE chapter_memory_facts ADD COLUMN kind TEXT NOT NULL DEFAULT 'state'")
        # The EPUB asset table was introduced after the first v0.4 builds. Keep
        # existing local libraries readable when the new chapter mapping column
        # is added without requiring a destructive database reset.
        asset_columns = {row[1] for row in conn.execute("PRAGMA table_info(epub_assets)")}
        if "chapter_idx" not in asset_columns:
            conn.execute("ALTER TABLE epub_assets ADD COLUMN chapter_idx INTEGER NOT NULL DEFAULT 0")
        # Older imports displayed unique assets at the chapter end. Seed that
        # equivalent occurrence only for chapters with no new anchor records.
        conn.execute("""
            INSERT OR IGNORE INTO epub_illustrations(book_id,chapter_idx,ordinal,before_para,asset_id,alt)
            SELECT a.book_id,a.chapter_idx,
                   ROW_NUMBER() OVER (PARTITION BY a.book_id,a.chapter_idx ORDER BY a.id)-1,
                   COALESCE(json_array_length(c.paras_json),0),a.id,''
            FROM epub_assets a LEFT JOIN chapters c ON c.book_id=a.book_id AND c.idx=a.chapter_idx
            WHERE NOT EXISTS (SELECT 1 FROM epub_illustrations i WHERE i.book_id=a.book_id AND i.chapter_idx=a.chapter_idx)
        """)
        conn.commit()
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
