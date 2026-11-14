"""Private, local model-library management. No generation is performed here."""
import asyncio

from fastapi import APIRouter, Body

from ..ai import catalog
from ..manga.models import MangaError

router = APIRouter(prefix="/api/ai/catalog")


@router.get("")
def get_catalog():
    return catalog.public()


def _save(payload, identity=None):
    try:
        return {"model": catalog.save_model(payload, identity)}
    except MangaError:
        raise
    except ValueError as exc:
        raise MangaError(str(exc)) from None


@router.post("/models", status_code=201)
def create_model(payload: dict = Body(...)):
    return _save(payload)


@router.patch("/models/{identity}")
def update_model(identity: str, payload: dict = Body(...)):
    return _save(payload, identity)


@router.delete("/models/{identity}")
def delete_model(identity: str):
    catalog.delete_model(identity)
    return {"ok": True}


@router.delete("/vendors/{connection_id}")
def delete_vendor(connection_id: str):
    catalog.delete_vendor(connection_id)
    return {"ok": True}


@router.get("/vendors/{connection_id}/models")
async def remote_models(connection_id: str):
    return await asyncio.to_thread(catalog.remote_models, connection_id)


@router.post("/models/{identity}/probe")
async def probe_model(identity: str):
    return await asyncio.to_thread(catalog.probe, identity)


@router.post("/models/{identity}/pool")
def add_model_to_pool(identity: str, payload: dict | None = Body(default=None)):
    """Place a library model in its text or image routing pool.

    ``kind`` is optional and defaults to the vendor's kind.  The operation is
    idempotent, making it safe for an autosaving settings panel to retry.
    """
    payload = payload or {}
    if not isinstance(payload, dict) or set(payload) - {"kind"}:
        raise MangaError("模型池请求包含未知字段")
    try:
        return {"pool": catalog.add_to_pool(identity, payload.get("kind"))}
    except MangaError:
        raise
    except ValueError as exc:
        raise MangaError(str(exc)) from None
