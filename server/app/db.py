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
        conn.commit()
    finally:
        conn.close()


def get_conn() -> sqlite3.Connection:
    """新建连接：Row 工厂 + 外键 + 30s 忙等（WAL 并发写）。调用方负责 close。"""
    conn = sqlite3.connect(DB_PATH, timeout=30)
    conn.row_factory = sqlite3.Row
    conn.execute("PRAGMA foreign_keys=ON")
    return conn
