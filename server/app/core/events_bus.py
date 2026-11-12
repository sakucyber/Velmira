"""进程内书库变更广播器（契约 8.2）。

部署假设 uvicorn 单 worker：revision 进程内单调递增，订阅者 = asyncio.Queue 集合。
- 订阅/退订在事件循环线程内完成（SSE 生成器）；
- publish 允许从任意线程调用：同线程直接 put_nowait，跨线程用
  loop.call_soon_threadsafe，保证 asyncio.Queue 不被跨线程直接操作；
- 队列满（慢客户端）时丢最旧事件，避免无界内存增长；
- 客户端断开由 SSE 生成器的 finally 负责 unsubscribe，不泄漏。
"""
from __future__ import annotations

import asyncio
import json
import threading

# 订阅队列上限：书库变更事件很稀疏，100 足够；满了丢最旧，防止慢客户端占内存
_QUEUE_MAXSIZE = 100


class EventsBus:
    """书库变更广播总线（进程内单例）。"""

    def __init__(self) -> None:
        self._revision = 0
        self._subscribers: dict[asyncio.Queue, asyncio.AbstractEventLoop] = {}
        self._lock = threading.Lock()

    @property
    def revision(self) -> int:
        with self._lock:
            return self._revision

    @property
    def subscriber_count(self) -> int:
        with self._lock:
            return len(self._subscribers)

    # -- 订阅管理（事件循环线程内调用）--------------------------------------
    def subscribe(self) -> asyncio.Queue:
        loop = asyncio.get_running_loop()
        queue: asyncio.Queue = asyncio.Queue(maxsize=_QUEUE_MAXSIZE)
        with self._lock:
            self._subscribers[queue] = loop
        return queue

    def unsubscribe(self, queue: asyncio.Queue) -> None:
        with self._lock:
            self._subscribers.pop(queue, None)

    # -- 广播 ----------------------------------------------------------------
    def publish_books_changed(self, count: int) -> int:
        """书库变化：revision+1 并向所有订阅者投递 books_changed。返回新 revision。

        事件格式（契约 8.2）：
            event: books_changed
            data: {"revision": N, "count": M}
            <空行>
        """
        with self._lock:
            self._revision += 1
            revision = self._revision
            payload = (
                "event: books_changed\n"
                'data: {"revision": %d, "count": %d}\n\n' % (revision, int(count))
            )
            subscribers = list(self._subscribers.items())

        try:
            running_loop = asyncio.get_running_loop()
        except RuntimeError:
            running_loop = None

        for queue, loop in subscribers:
            def _put(q=queue, payload=payload) -> None:
                if q.full():
                    try:
                        q.get_nowait()  # 慢客户端：丢最旧
                    except asyncio.QueueEmpty:
                        pass
                q.put_nowait(payload)

            try:
                if loop is running_loop:
                    _put()
                else:
                    loop.call_soon_threadsafe(_put)
            except RuntimeError:
                # loop 已关闭（进程收尾/测试环境）：顺手清理该订阅，避免残留
                self.unsubscribe(queue)
        return revision


# 进程内全局单例
events_bus = EventsBus()
