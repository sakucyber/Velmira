#!/usr/bin/env python
"""旧库升级检查辅助（verifier-m1）：DATA_DIR 指向复制来的老库，init_db 两次幂等，查新表。"""
import os
import sqlite3
import sys

os.environ.setdefault("DATA_DIR", sys.argv[1] if len(sys.argv) > 1 else "")
sys.path.insert(0, os.getcwd())  # 允许从项目根以 python tests/... 方式运行
from server.app.db import init_db, get_conn  # noqa: E402

init_db()
init_db()
conn = get_conn()
tables = sorted(r[0] for r in conn.execute("SELECT name FROM sqlite_master WHERE type='table'"))
books = conn.execute("SELECT COUNT(*) AS c FROM books").fetchone()["c"]
ann = conn.execute("SELECT COUNT(*) AS c FROM annotations").fetchone()["c"]
pages = conn.execute("SELECT COUNT(*) AS c FROM book_pages").fetchone()["c"]
print("TABLES=" + ",".join(tables))
print("HAS_NEW_TABLES=" + str({"book_pages", "annotations"}.issubset(set(tables))))
print("BOOKS=" + str(books) + " ANNOTATIONS=" + str(ann) + " BOOK_PAGES=" + str(pages))
conn.close()
print("INIT_DB_TWICE_OK")
