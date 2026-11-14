"""Per-endpoint request budgets shared by all books in this process."""
from collections import deque
from contextlib import contextmanager
import threading
import time

_condition = threading.Condition()
_budgets = {}


class RequestSlotBusy(TimeoutError):
    error_code = "capacity"


def wait_for_capacity(timeout):
    with _condition:
        _condition.wait(timeout)


@contextmanager
def request_slot(key, rpm=0, concurrency=1, *, wait_timeout=None):
    deadline = None if wait_timeout is None else time.monotonic() + wait_timeout
    with _condition:
        state = _budgets.setdefault(key, {"active": 0, "starts": deque()})
        while True:
            now = time.monotonic()
            while state["starts"] and now - state["starts"][0] >= 60:
                state["starts"].popleft()
            if state["active"] < concurrency and (not rpm or len(state["starts"]) < rpm):
                state["active"] += 1
                state["starts"].append(now)
                break
            if deadline is not None and now >= deadline:
                raise RequestSlotBusy("连接繁忙或达到请求上限")
            wait = max(.05, 60 - (now - state["starts"][0])) if rpm and len(state["starts"]) >= rpm else 1
            if deadline is not None:
                wait = min(wait, max(0, deadline - now))
            _condition.wait(min(wait, 1))
    try:
        yield
    finally:
        with _condition:
            state["active"] -= 1
            _condition.notify_all()
