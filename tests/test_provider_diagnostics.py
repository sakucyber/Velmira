from __future__ import annotations

import io
import unittest
import urllib.error
from types import SimpleNamespace
from unittest.mock import patch

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
