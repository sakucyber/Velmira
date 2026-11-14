import json
from fastapi import APIRouter, Body
from ..manga import storyboard as b, service as s
from ..manga.models import MangaError
from ..ai.models import stable_json
from .manga import connection, fields

router = APIRouter(prefix="/api/books/{book_id}/manga/storyboards")


@router.post("")
async def create(book_id: str, payload: dict = Body(...)):
    fields(payload, {"scene_id"})
    if not isinstance(payload.get("scene_id"), str):
        raise MangaError("请选择场景")
    return {"storyboard": await b.create(book_id, payload["scene_id"])}


@router.get("")
def listing(book_id: str):
    with connection(book_id) as conn:
        return {"storyboards": [dict(r) for r in conn.execute("SELECT id,scene_id,revision,updated_at FROM storyboards WHERE book_id=? ORDER BY updated_at DESC LIMIT 100", (book_id,))]}


@router.get("/{board_id}")
def get(book_id: str, board_id: str):
    with connection(book_id) as conn:
        return {"storyboard": b.get(conn, book_id, board_id)}


@router.patch("/{board_id}")
def update(book_id: str, board_id: str, payload: dict = Body(...)):
    fields(payload, {"revision", "panels"})
    with connection(book_id) as conn:
        auto = conn.execute("SELECT state_json FROM auto_manga WHERE book_id=?", (book_id,)).fetchone()
        state = json.loads(auto[0]) if auto else {}
        if state.get("board_id") == board_id and any(p.get("job_id") for p in state.get("story", [])):
            raise MangaError("自动页已开始出图，请先完成当前页；可在独立分镜中重新编排", "storyboard_in_use", 409)
        return {"storyboard": b.update(conn, book_id, board_id, payload.get("revision"), payload.get("panels"))}


@router.post("/{board_id}/render")
def render(book_id: str, board_id: str, payload: dict = Body(...)):
    fields(payload, {"revision", "panels"})
    with connection(book_id) as conn:
        board = b.get(conn, book_id, board_id)
        if isinstance(payload.get("revision"), bool) or payload.get("revision") != board["revision"]:
            raise MangaError("分镜版本已变化，请刷新", "revision_conflict", 409)
        indices = payload.get("panels", list(range(len(board["panels"]))))
        if not isinstance(indices, list) or not 1 <= len(indices) <= 7 or len(set(str(i) for i in indices)) != len(indices):
            raise MangaError("请选择1到7个不同画格")
        return {"jobs": [s.public_job(conn, b.enqueue(conn, book_id, board_id, i)) for i in indices]}


@router.post("/{board_id}/approve")
def approve(book_id: str, board_id: str, payload: dict = Body(...)):
    fields(payload, {"revision"})
    with connection(book_id) as conn:
        board = b.get(conn, book_id, board_id)
        row = conn.execute("SELECT * FROM auto_manga WHERE book_id=?", (book_id,)).fetchone()
        state = json.loads(row["state_json"]) if row else {}
        if state.get("board_id") != board_id or isinstance(payload.get("revision"), bool) or payload.get("revision") != board["revision"]:
            raise MangaError("自动分镜版本已变化，请刷新", "revision_conflict", 409)
        state["approved_revision"] = board["revision"]
        conn.execute("UPDATE auto_manga SET state_json=? WHERE book_id=?", (stable_json(state), book_id))
        return {"ok": True}
