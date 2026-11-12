"""实时书库 SSE（契约 8.2）。

- GET /api/events          → text/event-stream 长连接，事件 books_changed，
                             data {"revision": N, "count": M}；每 15s 发 ": ping"。
- GET /api/events/status   → {"revision", "subscribers"}（验收/排障辅助，契约未禁止）。

客户端断开/任务取消 → 生成器 finally 里 unsubscribe，队列不泄漏。
"""
from __future__ import annotations

import asyncio

from fastapi import APIRouter
from fastapi.responses import StreamingResponse

from ..core.events_bus import events_bus

router = APIRouter()

HEARTBEAT_SECONDS = 15

_SSE_HEADERS = {
    "Cache-Control": "no-cache",
    "Connection": "keep-alive",
    "X-Accel-Buffering": "no",
}


async def _event_stream():
    queue = events_bus.subscribe()
    try:
        # 立即回一帧注释行：curl/代理可马上确认连接已建立（注释不触发前端事件）
        yield ": ping\n\n"
        while True:
            try:
                payload = await asyncio.wait_for(queue.get(), timeout=HEARTBEAT_SECONDS)
            except asyncio.TimeoutError:
                yield ": ping\n\n"
            else:
                yield payload
    finally:
        events_bus.unsubscribe(queue)


@router.get("/api/events")
async def stream_events():
    return StreamingResponse(
        _event_stream(),
        media_type="text/event-stream",
        headers=_SSE_HEADERS,
    )


@router.get("/api/events/status")
def events_status():
    return {"revision": events_bus.revision, "subscribers": events_bus.subscriber_count}
