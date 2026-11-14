"""One retry budget per text request, including structured-output validation."""
import asyncio

from . import diagnostics

MAX_ATTEMPTS = 3
RETRY_DELAY = 1


def _hook(callback, exc, raw):
    # Diagnostic persistence must never replace the real failure.
    if not callback:
        return
    try:
        callback(exc, raw)
    except Exception:
        pass


async def complete(provider, *, validate=lambda value: value, guard=lambda: None,
                   report=None, label="文本 AI", attempt_aware=False, on_failure=None, **request):
    """Call one provider, validating structured output within the same budget.

    on_failure(exc, raw) runs for every failed request or validation so the
    caller can retain a bounded, redacted local sample.  When attempt_aware is
    true the validator receives the 1-based attempt number; chapter memory uses
    it to relax strict quotation checks only on the final attempt.
    """
    for attempt in range(1, MAX_ATTEMPTS + 1):
        guard()
        if report:
            report(f"{label} · 第 {attempt}/{MAX_ATTEMPTS} 次尝试")
        try:
            actual = {**request}
            if callable(actual.get("user")):
                actual["user"] = actual["user"]()
            # Inspect the class so AsyncMock/dynamic __getattr__ providers keep
            # their existing complete() contract. Routed providers can reject a
            # structurally invalid response before selecting a fallback model.
            validated = getattr(type(provider), "complete_validated", None)
            if callable(validated):
                raw = await validated(provider, validate=validate, **actual)
            else:
                raw = await provider.complete(**actual)
        except Exception as exc:
            guard()
            retryable = not diagnostics.non_retryable(exc)
            if report:
                if retryable:
                    report(f"{label} · 第 {attempt}/{MAX_ATTEMPTS} 次请求失败" + ("，即将重试" if attempt < MAX_ATTEMPTS else "，已停止"),
                           "warning" if attempt < MAX_ATTEMPTS else "error")
                else:
                    report(f"{label} · 第 {attempt}/{MAX_ATTEMPTS} 次请求失败，配置或凭据问题不再重试", "error")
            _hook(on_failure, exc, getattr(exc, "raw_preview", None))
            if attempt == MAX_ATTEMPTS or not retryable:
                raise
        else:
            guard()
            try:
                value = validate(raw, attempt) if attempt_aware else validate(raw)
            except Exception as exc:
                if report:
                    report(f"{label} · 第 {attempt}/{MAX_ATTEMPTS} 次输出无效" + ("，即将重试" if attempt < MAX_ATTEMPTS else "，已停止"),
                           "warning" if attempt < MAX_ATTEMPTS else "error")
                _hook(on_failure, exc, raw)
                if attempt == MAX_ATTEMPTS:
                    raise
            else:
                if report:
                    report(label + " · 完成")
                return value
        await asyncio.sleep(RETRY_DELAY * attempt)
