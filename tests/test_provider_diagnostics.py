from __future__ import annotations

import io
import unittest
import urllib.error
from types import SimpleNamespace
from unittest.mock import AsyncMock, patch

from server.app.ai.diagnostics import provider_failure
from server.app.ai.providers import ProviderRequestError, request_json
from server.app.ai import routing
from server.app.manga.provider import ImageAPIProvider


class ProviderDiagnosticsTests(unittest.TestCase):
    def test_http_statuses_map_to_actionable_categories(self):
        cases = {401: "provider_auth", 403: "provider_forbidden", 402: "provider_billing",
                 429: "provider_rate_limit", 500: "provider_server_error", 503: "provider_server_error"}
        for status, expected in cases.items():
            self.assertEqual(provider_failure(SimpleNamespace(status_code=status)), expected)

    def test_request_error_keeps_status_without_upstream_body(self):
        body = b"Bearer sk-secret upstream details"
        error = urllib.error.HTTPError("https://example.invalid/v1", 401, "unauthorized", {}, io.BytesIO(body))
        with patch("urllib.request.build_opener") as opener:
            opener.return_value.open.side_effect = error
            with self.assertRaises(ProviderRequestError) as raised:
                request_json("https://example.invalid/v1", api_key="sk-secret")
        self.assertEqual(raised.exception.status_code, 401)
        self.assertEqual(raised.exception.error_code, "http_error")
        self.assertNotIn("sk-secret", str(raised.exception))
        self.assertNotIn("upstream", str(raised.exception))

    def test_image_provider_keeps_safe_http_category(self):
        config = {"base_url": "https://example.invalid/v1", "model": "image"}
        for status, expected in [(401, "provider_auth"), (429, "provider_rate_limit")]:
            with patch.object(routing, "is_enabled", return_value=False), patch.object(ImageAPIProvider, "_generate_once", side_effect=ProviderRequestError("safe", status_code=status, error_code="http_error")):
                with self.assertRaises(ValueError) as raised:
                    ImageAPIProvider(config).generate({}, None, "request")
            self.assertEqual(provider_failure(raised.exception), expected)

    def test_failure_detail_explains_reasoning_budget(self):
        from server.app.ai.diagnostics import failure_detail
        error = ProviderRequestError("推理模型在思考阶段用完了最大 Token", error_code="reasoning_budget",
                                     finish_reason="length", max_tokens=8000,
                                     usage={"completion_tokens": 8000, "completion_tokens_details": {"reasoning_tokens": 8000}})
        info = failure_detail(error, stage="memory", chapter=2, model="deepseek-flash")
        self.assertEqual(info["reason"], "reasoning_budget")
        self.assertEqual(info["code"], "output_reasoning_budget")
        self.assertEqual(info["stage"], "memory")
        self.assertEqual(info["chapter"], 2)
        self.assertEqual(info["finish_reason"], "length")
        self.assertEqual(info["usage"]["reasoning_tokens"], 8000)
        self.assertIn("第 3 章章节记忆阶段失败", info["message"])
        self.assertIn("思考 8000", info["message"])
        self.assertIn("16000", info["suggestion"])
        self.assertNotIn("sk-", str(info))

    def test_reasoning_effort_and_hard_token_ceiling(self):
        from server.app.ai.providers import HARD_MAX_OUTPUT_TOKENS, OpenAICompatProvider
        provider = OpenAICompatProvider("https://text.example/v1", "reasoner")
        cfg = {"base_url": "https://text.example/v1", "model": "reasoner", "reasoning_effort": "low",
               "max_tokens": 16000, "temperature": 0.0, "json_mode": True}
        with patch("server.app.ai.providers.request_json",
                   return_value={"choices": [{"message": {"content": '{"ok":true}'}, "finish_reason": "stop"}],
                                 "usage": {"completion_tokens": 10}}) as request:
            provider._complete_once("system", "user", {"type": "object"}, .2, cfg)
        body = request.call_args.kwargs["body"]
        self.assertEqual(body["reasoning_effort"], "low")
        self.assertEqual(body["max_tokens"], 16000)
        self.assertEqual(body["temperature"], 0.0)
        # The v0.4.6 report said 32000 was never sent; it is valid here.
        with patch("server.app.ai.providers.request_json",
                   return_value={"choices": [{"message": {"content": '{"ok":true}'}, "finish_reason": "stop"}]}) as request:
            provider._complete_once("system", "user", {"type": "object"}, .2, {**cfg, "max_tokens": 32000})
        self.assertEqual(request.call_args.kwargs["body"]["max_tokens"], 32000)
        with self.assertRaises(ProviderRequestError) as raised:
            provider._complete_once("system", "user", {"type": "object"}, .2,
                                    {**cfg, "max_tokens": HARD_MAX_OUTPUT_TOKENS + 1})
        self.assertEqual(raised.exception.error_code, "invalid_config")
        with patch("server.app.ai.providers.request_json", return_value={"choices": [
                {"message": {"content": ""}, "finish_reason": "length"}],
                "usage": {"completion_tokens": 16000, "completion_tokens_details": {"reasoning_tokens": 16000}}}):
            with self.assertRaises(ProviderRequestError) as raised:
                provider._complete_once("system", "user", {"type": "object"}, .2, cfg)
        self.assertEqual(raised.exception.error_code, "reasoning_budget")
        self.assertEqual(raised.exception.usage["reasoning_tokens"], 16000)
        self.assertEqual(raised.exception.max_tokens, 16000)

    def test_retry_stops_immediately_for_configuration_failures(self):
        import asyncio
        from server.app.ai import retry
        provider = SimpleNamespace(complete=AsyncMock(
            side_effect=ProviderRequestError("unauthorized", status_code=401, error_code="http_error")))
        with self.assertRaises(ProviderRequestError):
            asyncio.run(retry.complete(provider, system="s", user="u"))
        self.assertEqual(provider.complete.await_count, 1)

    def test_routing_exhaustion_keeps_category_only(self):
        cfg = {"enabled": True, "models": [{"connection_id": "c", "model": "m"}], "timeout": 1,
               "temperature": .2, "max_tokens": 100, "cooldown": 0, "strategy": "ordered", "revision": 1}
        candidate = {"connection_id": "c", "model": "m", "base_url": "https://example.invalid/v1", "max_concurrency": 1, "rpm": 1}
        with patch.object(routing, "get_config", return_value=cfg), patch.object(routing.connections, "resolve", return_value=candidate), patch.object(routing, "_telemetry"):
            with self.assertRaises(routing.RoutingExhausted) as raised:
                routing.run("image", lambda _: (_ for _ in ()).throw(ProviderRequestError("Bearer sk-secret", status_code=429, error_code="arbitrary-sk-secret")))
        self.assertEqual(provider_failure(raised.exception), "provider_rate_limit")
        self.assertEqual(raised.exception.error_code, "provider_rate_limit")
        self.assertNotIn("sk-secret", str(raised.exception))


if __name__ == "__main__":
    unittest.main()
