"""One retry budget per text request, including structured-output validation."""
import asyncio

MAX_ATTEMPTS = 3
RETRY_DELAY = 1


async def complete(provider, *, validate=lambda value: value, guard=lambda: None,
                   report=None, label="文本 AI", **request):
    for attempt in range(1, MAX_ATTEMPTS + 1):
        guard()
        if report:
            report(f"{label} · 第 {attempt}/{MAX_ATTEMPTS} 次尝试")
        try:
            actual = {**request}
            if callable(actual.get("user")):
                actual["user"] = actual["user"]()
            raw = await provider.complete(**actual)
        except Exception:
            guard()
            if report:
                report(f"{label} · 第 {attempt}/{MAX_ATTEMPTS} 次请求失败" + ("，即将重试" if attempt < MAX_ATTEMPTS else "，已停止"), "warning" if attempt < MAX_ATTEMPTS else "error")
            if attempt == MAX_ATTEMPTS:
                raise
        else:
            guard()
            try:
                value = validate(raw)
            except Exception:
                if report:
                    report(f"{label} · 第 {attempt}/{MAX_ATTEMPTS} 次输出无效" + ("，即将重试" if attempt < MAX_ATTEMPTS else "，已停止"), "warning" if attempt < MAX_ATTEMPTS else "error")
                if attempt == MAX_ATTEMPTS:
                    raise
            else:
                if report:
                    report(label + " · 完成")
                return value
        await asyncio.sleep(RETRY_DELAY * attempt)
