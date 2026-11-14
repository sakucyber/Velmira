from fastapi import APIRouter, Body, Query, Request

from .. import security
from ..ai import features, memory, web_search, settings as credentials
from ..manga.models import MangaError

router = APIRouter()


def _settings_response(request, value):
    # Only the local settings form receives the saved search key. Creative
    # snapshots and other callers of features.public() remain credential-free.
    if security.is_local(request):
        stored = credentials.load_settings("web-search")
        value["search"]["api_key"] = (credentials.api_key(stored)
                                      if stored["base_url"] == value["search"]["base_url"] else "")
    return {"settings": value}


@router.get("/api/ai/features")
def get_features(request: Request):
    return _settings_response(request, features.public())


@router.patch("/api/ai/features")
def patch_features(request: Request, payload: dict = Body(...)):
    return _settings_response(request, features.save(payload))


@router.post("/api/ai/web-search")
def search_web(payload: dict = Body(...)):
    if set(payload) != {"query"}:
        raise MangaError("请仅提供 query")
    return web_search.search(payload["query"])


@router.get("/api/ai/memories")
def list_memories(book_id: str | None = None, q: str = Query(default="", max_length=200)):
    return memory.list_all(book_id, q)


@router.post("/api/ai/memories", status_code=201)
def create_memory(payload: dict = Body(...)):
    return {"memory": memory.save(payload)}


@router.patch("/api/ai/memories/{memory_id}")
def edit_memory(memory_id: str, payload: dict = Body(...)):
    return {"memory": memory.save(payload, memory_id)}


@router.delete("/api/ai/memories/{memory_id}")
def delete_memory(memory_id: str, revision: int = Query(...)):
    memory.delete(memory_id, revision)
    return {"ok": True}
