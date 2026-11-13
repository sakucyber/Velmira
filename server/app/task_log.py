"""Bounded task events. Never store prompts, credentials or provider error bodies."""
import time

from . import db


def record(conn, book_id, run_id, stage, message, level="info"):
    conn.execute("INSERT INTO task_logs(book_id,run_id,stage,level,message,created_at) VALUES(?,?,?,?,?,?)",
                 (book_id, run_id, stage, level, message, int(time.time())))
    conn.execute("DELETE FROM task_logs WHERE book_id=? AND id NOT IN (SELECT id FROM task_logs WHERE book_id=? ORDER BY id DESC LIMIT 1000)", (book_id, book_id))


def reporter(book_id, run_id, stage):
    def report(message, level="info"):
        conn = db.get_conn()
        try:
            with conn:
                record(conn, book_id, run_id, stage, message, level)
        finally:
            conn.close()
    return report
