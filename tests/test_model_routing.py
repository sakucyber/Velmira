"""Model routing uses disposable connections and never calls a remote service."""
from concurrent.futures import ThreadPoolExecutor
import asyncio
import base64
import importlib
import io
import json
from pathlib import Path
import sqlite3
import tempfile
import threading
import unittest
import urllib.error
from unittest.mock import patch

from fastapi.testclient import TestClient
from PIL import Image

from server.app import db
from server.app.ai import book_analysis, connections, director, limits, retry, routing, settings
from server.app.ai.providers import OpenAICompatProvider
from server.app.main import create_app
from server.app.manga.provider import ImageAPIProvider


class ModelRoutingTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        for target, value in [("server.app.config.DATA_DIR", self.root),
                              ("server.app.db.DB_PATH", self.root / "app.db"),
                              ("server.app.ai.limits._budgets", {})]:
            mocked = patch(target, value)
            mocked.start()
            self.addCleanup(mocked.stop)
        db.init_db()
        self.client = TestClient(create_app())
        self.addCleanup(self.client.close)
        self.text = [self.connection("text", "Text " + str(index), "text-" + str(index), index)
                     for index in range(3)]
        self.image = [self.connection("image", "Image " + str(index), "image-" + str(index), index,
                                      reference_api="generations" if index else "edits")
                      for index in range(2)]

    def connection(self, kind, label, model, number, **extras):
        return connections.save_connection({
            "label": label, "kind": kind, "models": [model],
            "base_url": "https://" + kind + str(number) + ".example/v1",
            "api_key": "disposable-credential-" + kind + str(number),
            "rpm": 25, "max_concurrency": 5 if kind == "text" else 8,
            "json_mode": number != 1, "reference_api": "edits", **extras,
        })

    def members(self, entries):
        return [{"connection_id": entry["id"], "model": entry["models"][0]}
                for entry in entries]

    def enable(self, kind="text", entries=None, **values):
        return routing.save_config(kind, {
            "enabled": True, "models": self.members(entries or (self.text if kind == "text" else self.image)),
            "strategy": "ordered", "cooldown": 0, **values,
        })

    def test_disabled_routing_preserves_single_connection(self):
        self.assertFalse(routing.is_enabled("text"))
        fallback = {"model": "single-model", "base_url": "https://single.example/v1"}
        seen = []
        result = routing.run("text", lambda candidate: seen.append(candidate) or "single-result", fallback=fallback)
        self.assertEqual(result, "single-result")
        self.assertEqual(seen, [fallback])

    def test_presets_apply_only_when_selected_and_keep_advanced_overrides(self):
        # Existing files have no preset field. Adding the label during a load
        # must not silently replace their custom timeout or cooldown.
        routing._path("text").write_text(json.dumps({"timeout": 75, "cooldown": 7}), encoding="utf-8")
        legacy = routing.get_config("text")
        self.assertEqual((legacy["preset"], legacy["timeout"], legacy["cooldown"]), ("balanced", 75, 7))
        changed = routing.save_config("text", {"strategy": "random"})
        self.assertEqual((changed["timeout"], changed["cooldown"]), (75, 7))
        selected = routing.save_config("text", {"preset": "stable", "timeout": 95})
        self.assertEqual((selected["timeout"], selected["cooldown"]), (95, 120))
        self.assertEqual(routing.save_config("text", {"temperature": .9})["timeout"], 95)
        fast = routing.save_config("text", {"preset": "fast"})
        self.assertEqual((fast["timeout"], fast["cooldown"]), (60, 15))
        for invalid in ("unknown", {}, None):
            with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                routing.save_config("text", {"preset": invalid})

    def test_config_metadata_is_grouped_and_contains_no_private_settings(self):
        domains = self.client.get("/api/config/domains").json()["domains"]
        self.assertEqual([item["id"] for item in domains], ["reader", "ai", "extensions", "system"])
        presets = self.client.get("/api/config/presets").json()
        self.assertEqual([item["id"] for item in presets["text"]], ["stable", "balanced", "fast"])
        self.assertNotIn("disposable-credential", json.dumps(presets))

    def test_ordered_failure_uses_next_model_and_records_logical_request(self):
        self.enable()
        called = []

        def request(candidate):
            called.append(candidate)
            if candidate["model"] == "text-0":
                raise TimeoutError("private upstream body disposable-credential-text0")
            return "fallback-result"

        self.assertEqual(routing.run("text", request), "fallback-result")
        self.assertEqual([candidate["model"] for candidate in called], ["text-0", "text-1"])
        self.assertEqual(called[1]["credential_slot"], "profile-" + self.text[1]["id"])
        self.assertEqual(called[1]["base_url"], "https://text1.example/v1")
        self.assertEqual(settings.api_key(settings.load_settings(called[1]["credential_slot"])),
                         "disposable-credential-text1")
        status = routing.public("text")["status"]
        self.assertEqual(status["requests"], 1)
        self.assertEqual(status["attempts"], 2)
        by_model = {item["model"]: item for item in status["models"]}
        self.assertEqual(by_model["text-0"]["failures"], 1)
        self.assertEqual(by_model["text-1"]["successes"], 1)
        self.assertNotIn("private upstream", json.dumps(status))
        self.assertNotIn("disposable-credential", json.dumps(status))

    def test_round_robin_changes_start_without_waiting_for_failures(self):
        self.enable(strategy="round_robin")
        selected = [routing.run("text", lambda candidate: candidate["model"]) for _ in range(6)]
        self.assertEqual(selected, ["text-0", "text-1", "text-2"] * 2)

    def test_round_robin_start_is_atomic_for_concurrent_requests(self):
        self.enable(strategy="round_robin")
        start = threading.Barrier(9)

        def work(_):
            start.wait(timeout=5)
            return routing.run("text", lambda candidate: candidate["model"])

        with ThreadPoolExecutor(max_workers=9) as pool:
            selected = list(pool.map(work, range(9)))
        self.assertEqual({model: selected.count(model) for model in set(selected)},
                         {"text-0": 3, "text-1": 3, "text-2": 3})
        self.assertEqual(routing.public("text")["status"]["attempts"], 9)

    def test_random_strategy_tries_every_candidate_at_most_once(self):
        self.enable(strategy="random")
        called = []

        def failing(candidate):
            called.append(candidate["model"])
            raise RuntimeError("simulated unavailable")

        with self.assertRaises(routing.RoutingExhausted):
            routing.run("text", failing)
        self.assertEqual(len(called), 3)
        self.assertEqual(set(called), {"text-0", "text-1", "text-2"})

    def test_failure_cooldown_skips_model_then_allows_recovery(self):
        self.enable(entries=self.text[:2], cooldown=60)
        clock = [1_800_000_000.0]
        called = []

        def request(candidate):
            called.append(candidate["model"])
            if candidate["model"] == "text-0" and clock[0] < 1_800_000_061:
                raise TimeoutError()
            return candidate["model"]

        with patch.object(routing.time, "time", side_effect=lambda: clock[0]):
            self.assertEqual(routing.run("text", request), "text-1")
            self.assertEqual(routing.run("text", request), "text-1")
            self.assertEqual(called, ["text-0", "text-1", "text-1"])
            self.assertGreater(routing.public("text")["status"]["models"][0]["cooldown_remaining"], 0)
            clock[0] += 61
            self.assertEqual(routing.run("text", request), "text-0")
            self.assertEqual(routing.public("text")["status"]["models"][0]["cooldown_remaining"], 0)

    def test_all_cooling_models_fail_fast_and_do_not_call_endpoint_again(self):
        self.enable(entries=self.text[:2], cooldown=60)
        called = []

        def failing(candidate):
            called.append(candidate["model"])
            raise RuntimeError("private-error")

        with self.assertRaises(routing.RoutingExhausted):
            routing.run("text", failing)
        with self.assertRaises(routing.RoutingExhausted) as caught:
            routing.run("text", failing)
        self.assertEqual(called, ["text-0", "text-1"])
        self.assertNotIn("private-error", str(caught.exception))

    def test_disabling_cooldown_allows_immediate_retry_of_recovered_model(self):
        self.enable(entries=self.text[:1], cooldown=60)
        def unavailable(candidate):
            raise TimeoutError()
        with self.assertRaises(routing.RoutingExhausted):
            routing.run("text", unavailable)
        self.assertGreater(routing.public("text")["status"]["models"][0]["cooldown_remaining"], 0)
        routing.save_config("text", {"cooldown": 0})
        self.assertEqual(routing.run("text", lambda candidate: "recovered"), "recovered")
        self.assertEqual(routing.public("text")["status"]["models"][0]["cooldown_remaining"], 0)

    def test_image_candidates_keep_distinct_reference_protocol_and_limits(self):
        self.enable("image", timeout=240)
        seen = []

        def request(candidate):
            seen.append(candidate)
            if len(seen) == 1:
                raise RuntimeError("unavailable")
            return b"image-result"

        self.assertEqual(routing.run("image", request), b"image-result")
        self.assertEqual([candidate["reference_api"] for candidate in seen], ["edits", "generations"])
        self.assertTrue(all(candidate["rpm"] == 25 and candidate["max_concurrency"] == 8
                            and candidate["timeout"] == 240 for candidate in seen))
        self.assertTrue(all("api_key" not in candidate and "key_dpapi" not in candidate
                            and "key_local" not in candidate for candidate in seen))

    def test_pool_parameters_apply_to_each_text_candidate(self):
        self.enable(temperature=0.7, max_tokens=4096, timeout=240)
        candidate = routing.run("text", lambda value: value)
        self.assertEqual(candidate["temperature"], 0.7)
        self.assertEqual(candidate["max_tokens"], 4096)
        self.assertEqual(candidate["timeout"], 240)

    def test_statistics_failure_does_not_retry_a_successful_billed_request(self):
        self.enable()
        seen = []
        with patch.object(routing, "_database", side_effect=sqlite3.OperationalError("unavailable")):
            result = routing.run("text", lambda candidate: seen.append(candidate["model"]) or "success")
        self.assertEqual(result, "success")
        self.assertEqual(seen, ["text-0"])

    def test_statistics_failure_does_not_block_loading_or_saving_pool_settings(self):
        self.enable()
        with patch.object(routing, "_database", side_effect=sqlite3.OperationalError("unavailable")):
            loaded = self.client.get("/api/ai/routing/text")
            self.assertEqual(loaded.status_code, 200, loaded.text)
            self.assertFalse(loaded.json()["status"]["available"])
            saved = self.client.patch("/api/ai/routing/text", json={"strategy": "round_robin"})
            self.assertEqual(saved.status_code, 200, saved.text)
            self.assertFalse(saved.json()["status"]["available"])
            self.assertEqual(saved.json()["config"]["strategy"], "round_robin")
        self.assertEqual(routing.get_config("text")["strategy"], "round_robin")

    def test_busy_connection_uses_another_endpoint_without_failure_or_cooldown(self):
        self.enable(entries=self.text[:2], cooldown=60)
        seen = []

        def request(candidate):
            seen.append(candidate["model"])
            if candidate["model"] == "text-0":
                raise limits.RequestSlotBusy()
            return candidate["model"]

        self.assertEqual(routing.run("text", request), "text-1")
        self.assertEqual(seen, ["text-0", "text-1"])
        status = routing.public("text")["status"]
        first = status["models"][0]
        self.assertEqual(status["attempts"], 1)
        self.assertEqual(first["attempts"], 0)
        self.assertEqual(first["failures"], 0)
        self.assertEqual(first["cooldown_remaining"], 0)
        self.assertEqual(routing.run("text", lambda candidate: candidate["model"]), "text-0")

    def test_all_busy_connections_wait_for_capacity_then_resume(self):
        self.enable(entries=self.text[:2], timeout=5)
        available, seen = [False], []

        def request(candidate):
            seen.append(candidate["model"])
            if not available[0]:
                raise limits.RequestSlotBusy()
            return candidate["model"]

        with patch.object(limits, "wait_for_capacity", side_effect=lambda _timeout: available.__setitem__(0, True)) as wait:
            self.assertEqual(routing.run("text", request), "text-0")
        self.assertEqual(seen, ["text-0", "text-1", "text-0"])
        wait.assert_called_once()
        self.assertLessEqual(wait.call_args.args[0], 0.25)
        self.assertEqual(routing.public("text")["status"]["attempts"], 1)

    def test_capacity_wait_is_bounded_without_counting_network_failures(self):
        self.enable(entries=self.text[:2], timeout=5)
        clock, waits = [0.0], []

        def wait(timeout):
            waits.append(timeout)
            clock[0] += timeout

        def request(_candidate):
            raise limits.RequestSlotBusy()

        with patch.object(routing.time, "monotonic", side_effect=lambda: clock[0]), \
                patch.object(limits, "wait_for_capacity", side_effect=wait):
            with self.assertRaises(routing.RoutingExhausted):
                routing.run("text", request)
        self.assertEqual(sum(waits), 5)
        self.assertTrue(all(0 < timeout <= 0.25 for timeout in waits))
        status = routing.public("text")["status"]
        self.assertEqual(status["attempts"], 0)
        self.assertTrue(all(model["failures"] == 0 and model["cooldown_remaining"] == 0
                            for model in status["models"]))

    def test_zero_wait_limit_rejects_busy_concurrency_and_rpm_without_sleeping(self):
        with patch.object(limits._condition, "wait") as wait:
            with limits.request_slot("concurrency", 0, 1):
                with self.assertRaises(limits.RequestSlotBusy):
                    with limits.request_slot("concurrency", 0, 1, wait_timeout=0):
                        self.fail("A busy connection cannot be acquired")
            with limits.request_slot("rate", 1, 1, wait_timeout=0):
                pass
            with self.assertRaises(limits.RequestSlotBusy):
                with limits.request_slot("rate", 1, 1, wait_timeout=0):
                    self.fail("An exhausted rate window cannot be acquired")
            with limits.request_slot("independent", 1, 1, wait_timeout=0):
                pass
            wait.assert_not_called()
        self.assertEqual(limits._budgets["concurrency"]["active"], 0)
        self.assertEqual(limits._budgets["rate"]["active"], 0)

    def test_pool_changes_invalidate_director_and_book_analysis_configuration(self):
        settings.save_settings({"enabled": True, "base_url": "https://text0.example/v1",
                                "model": "text-0", "api_key": "active-disposable-secret"})
        self.enable()
        old_director, old_book = director.text_config(), book_analysis.current_config()
        routing.save_config("text", {"models": self.members(list(reversed(self.text)))})
        self.assertNotEqual(director.text_config(), old_director)
        self.assertNotEqual(book_analysis.current_config(), old_book)
        snapshot = json.dumps([director.text_config(), book_analysis.current_config()])
        self.assertNotIn("active-disposable-secret", snapshot)
        self.assertNotIn("disposable-credential", snapshot)

    def test_configured_pool_can_enable_text_without_a_single_connection(self):
        self.assertEqual(settings.load_settings()["base_url"], "")
        self.enable()
        enabled = self.client.patch("/api/ai/settings", json={"enabled": True, "auto_analyze": False})
        self.assertEqual(enabled.status_code, 200, enabled.text)
        self.assertTrue(enabled.json()["settings"]["enabled"])
        self.assertFalse(enabled.json()["settings"]["auto_analyze"])
        self.assertEqual(settings.load_settings()["base_url"], "")
        for configured in (director.text_config(), book_analysis.current_config()):
            self.assertEqual(configured["base_url"], "https://text0.example/v1")
            self.assertEqual(configured["model"], "text-0")

    def test_text_provider_switches_endpoint_credential_model_and_json_mode_together(self):
        self.enable(entries=self.text[:2], timeout=240, max_tokens=4096)
        provider = OpenAICompatProvider("https://text0.example/v1", "text-0", local_settings=True)
        response = {"choices": [{"message": {"content": '{"ok":true}'}}]}
        with patch("server.app.ai.providers.request_json", side_effect=[TimeoutError(), response]) as request:
            result = provider._complete_sync("Only JSON", "neutral input", {"type": "object"}, 0.2)
        self.assertEqual(result, {"ok": True})
        first, second = request.call_args_list
        self.assertEqual(first.args[0], "https://text0.example/v1/chat/completions")
        self.assertEqual(second.args[0], "https://text1.example/v1/chat/completions")
        self.assertEqual(second.kwargs["api_key"], "disposable-credential-text1")
        self.assertEqual(second.kwargs["body"]["model"], "text-1")
        self.assertEqual(second.kwargs["body"]["max_tokens"], 4096)
        self.assertNotIn("response_format", second.kwargs["body"])
        self.assertEqual(second.kwargs["timeout"], 240)

    def test_business_validation_failure_falls_back_inside_one_text_retry(self):
        self.enable(entries=self.text[:2])
        provider = OpenAICompatProvider("https://text0.example/v1", "text-0", local_settings=True)
        responses = [{"choices": [{"message": {"content": content}}]}
                     for content in ('{}', '{"answer":"ok"}')]
        reports = []
        with patch("server.app.ai.providers.request_json", side_effect=responses) as request, \
                patch.object(retry, "RETRY_DELAY", 0):
            result = asyncio.run(retry.complete(provider, system="JSON", user="neutral input",
                                                schema={"type": "object"}, validate=lambda value: value["answer"],
                                                report=lambda message, *_: reports.append(message)))
        self.assertEqual(result, "ok")
        self.assertEqual(request.call_count, 2)
        self.assertEqual([call.kwargs["body"]["model"] for call in request.call_args_list],
                         ["text-0", "text-1"])
        self.assertFalse(any("第 2/" in message for message in reports))
        self.assertEqual(routing.public("text")["status"]["requests"], 1)

    def test_explicit_static_provider_is_not_redirected_by_global_pool(self):
        self.enable()
        provider = OpenAICompatProvider("https://explicit.example/v1", "explicit-model")
        response = {"choices": [{"message": {"content": '{"ok":true}'}}]}
        with patch("server.app.ai.providers.request_json", return_value=response) as request:
            self.assertEqual(provider._complete_sync("JSON", "neutral input", {"type": "object"}, 0.2),
                             {"ok": True})
        self.assertEqual(request.call_args.args[0], "https://explicit.example/v1/chat/completions")
        self.assertEqual(request.call_args.kwargs["body"]["model"], "explicit-model")
        self.assertEqual(routing.public("text")["status"]["requests"], 0)

    def test_image_provider_fallback_keeps_both_ordered_references(self):
        self.enable("image")
        paths = []
        for index, color in enumerate(("red", "green")):
            path = self.root / ("reference-" + str(index) + ".png")
            Image.new("RGB", (64, 64), color).save(path, "PNG")
            paths.append(path)
        image = io.BytesIO()
        Image.new("RGB", (64, 64), "blue").save(image, "PNG")
        payload = json.dumps({"data": [{"b64_json": base64.b64encode(image.getvalue()).decode()}]}).encode()
        seen = []

        class Response:
            def __enter__(self):
                return self

            def __exit__(self, *_):
                return False

            def read(self, _size):
                return payload

        class Opener:
            def open(self, request, timeout):
                seen.append(request)
                if len(seen) == 1:
                    raise urllib.error.HTTPError(request.full_url, 503, "unavailable", {}, None)
                return Response()

        provider = ImageAPIProvider({"base_url": "https://image0.example/v1", "model": "image-0",
                                     "width": 1024, "height": 1024, "timeout": 120,
                                     "reference_api": "edits", "rpm": 25, "max_concurrency": 8})
        with patch("server.app.manga.provider.urllib.request.build_opener", return_value=Opener()):
            result = provider.generate({"prompt": "Two adults pass a key."}, paths, "test-request")
        self.assertEqual(result, image.getvalue())
        self.assertEqual([request.full_url for request in seen],
                         ["https://image0.example/v1/images/edits", "https://image1.example/v1/images/generations"])
        second = json.loads(seen[1].data)
        self.assertEqual(second["model"], "image-1")
        self.assertEqual(second["image"], ["data:image/png;base64," + base64.b64encode(path.read_bytes()).decode()
                                           for path in paths])
        self.assertEqual(seen[1].get_header("Authorization"), "Bearer disposable-credential-image1")

    def test_image_provider_rejects_tiny_valid_png_before_accepting_fallback(self):
        self.enable("image")
        payloads, seen = [], []
        for size in (1, 64):
            output = io.BytesIO()
            Image.new("RGB", (size, size), "blue").save(output, "PNG")
            payloads.append(json.dumps({"data": [{"b64_json": base64.b64encode(output.getvalue()).decode()}]}).encode())

        class Response:
            def __init__(self, data):
                self.data = data

            def __enter__(self):
                return self

            def __exit__(self, *_):
                return False

            def read(self, _size):
                return self.data

        class Opener:
            def open(self, request, timeout):
                seen.append(request)
                return Response(payloads[len(seen) - 1])

        provider = ImageAPIProvider({"base_url": "https://image0.example/v1", "model": "image-0",
                                     "width": 1024, "height": 1024, "timeout": 120})
        with patch("server.app.manga.provider.urllib.request.build_opener", return_value=Opener()):
            result = provider.generate({"prompt": "A quiet greenhouse."}, None, "tiny-check")
        with Image.open(io.BytesIO(result)) as image:
            self.assertEqual(image.size, (64, 64))
        self.assertEqual(len(seen), 2)
        self.assertEqual(routing.public("image")["status"]["models"][0]["failures"], 1)

    def test_image_pool_probe_reports_backup_without_routing_statistics_or_cooldown(self):
        self.enable("image")

        def request(url, **_kwargs):
            if url == "https://image0.example/v1/models":
                raise urllib.error.HTTPError(url, 503, "private-gateway-body", {}, None)
            self.assertEqual(url, "https://image1.example/v1/models")
            return {"data": [{"id": "image-1"}]}

        provider = ImageAPIProvider({})
        with patch.object(routing, "run", side_effect=AssertionError("Probe cannot render")) as run, \
                patch("server.app.ai.providers.request_json", side_effect=request) as calls:
            result = provider.probe()
        run.assert_not_called()
        self.assertTrue(result["ok"])
        self.assertEqual([model["ok"] for model in result["models"]], [False, True])
        self.assertEqual(calls.call_count, 2)
        self.assertTrue(all(call.args[0].endswith("/models") and call.kwargs["timeout"] <= 10
                            for call in calls.call_args_list))
        self.assertNotIn("private-gateway-body", json.dumps(result))
        self.assertNotIn("disposable-credential", json.dumps(result))
        status = routing.public("image")["status"]
        self.assertEqual(status["requests"], 0)
        self.assertEqual(status["attempts"], 0)
        self.assertTrue(all(model["cooldown_remaining"] == 0 for model in status["models"]))

        # More than eight candidates still use a bounded real executor.
        expanded = connections.save_connection({"id": self.image[0]["id"], "label": "Image pool",
                                                "kind": "image", "models": ["image-" + str(i) for i in range(10)]})
        routing.save_config("image", {"models": [{"connection_id": expanded["id"], "model": model}
                                                   for model in expanded["models"]]})
        with patch("server.app.manga.provider.ThreadPoolExecutor", wraps=ThreadPoolExecutor) as executor, \
                patch("server.app.ai.providers.request_json", side_effect=request):
            result = provider.probe()
        executor.assert_called_once_with(max_workers=8)
        self.assertEqual(len(result["models"]), 10)
        self.assertEqual(routing.public("image")["status"]["requests"], 0)

    def test_statistics_survive_module_reload_and_expire_after_24_hours(self):
        self.enable()
        initial = 1_800_000_000
        with patch.object(routing.time, "time", return_value=initial):
            routing.run("text", lambda candidate: "ok")
            before = routing.public("text")["status"]
        importlib.reload(routing)
        with patch.object(routing.time, "time", return_value=initial + 1):
            restored = routing.public("text")["status"]
        self.assertEqual(restored["requests"], before["requests"])
        self.assertEqual(restored["attempts"], before["attempts"])
        with patch.object(routing.time, "time", return_value=initial + 86_401):
            expired = routing.public("text")["status"]
        self.assertEqual(expired["requests"], 0)
        self.assertEqual(expired["attempts"], 0)

    def test_config_validation_rejects_missing_wrong_kind_and_duplicate_models(self):
        valid = self.members(self.text[:1])
        invalid = [
            {"enabled": True, "models": []},
            {"enabled": True, "models": [{"connection_id": "missing", "model": "missing"}]},
            {"enabled": True, "models": self.members(self.image[:1])},
            {"enabled": True, "models": valid * 2},
            {"models": [{**valid[0], "model": "not-in-connection"}]},
            {"strategy": "unknown"}, {"enabled": 1}, {"timeout": True},
            {"timeout": 0}, {"cooldown": -1}, {"temperature": 3},
            {"temperature": "0.7"}, {"max_tokens": 0}, {"max_tokens": True},
            {"api_key": "never-store-in-routing"},
        ]
        for values in invalid:
            with self.subTest(values=values):
                response = self.client.patch("/api/ai/routing/text", json=values)
                self.assertEqual(response.status_code, 400, response.text)
        self.assertFalse(routing.is_enabled("text"))

    def test_api_optimistic_lock_prevents_losing_other_window_changes(self):
        original = self.client.get("/api/ai/routing/text")
        self.assertEqual(original.status_code, 200, original.text)
        revision = original.json()["config"]["revision"]
        first = self.client.patch("/api/ai/routing/text", json={
            "revision": revision, "models": self.members(self.text), "enabled": True,
            "strategy": "round_robin",
        })
        self.assertEqual(first.status_code, 200, first.text)
        self.assertGreater(first.json()["config"]["revision"], revision)
        stale = self.client.patch("/api/ai/routing/text", json={"revision": revision, "strategy": "random"})
        self.assertEqual(stale.status_code, 409, stale.text)
        self.assertEqual(routing.get_config("text")["strategy"], "round_robin")

    def test_connection_api_and_public_routing_never_return_credentials(self):
        secret = "only-this-test-secret"
        saved = self.client.post("/api/ai/connections", json={
            "label": "My backup", "kind": "text", "models": ["backup-model"],
            "base_url": "https://backup.example/v1", "api_key": secret,
            "rpm": 25, "max_concurrency": 5, "json_mode": False,
            "reference_api": "edits",
        })
        self.assertIn(saved.status_code, (200, 201), saved.text)
        self.assertNotIn(secret, saved.text)
        member = self.members([saved.json()["connection"]])
        response = self.client.patch("/api/ai/routing/text", json={"models": member, "enabled": True})
        self.assertEqual(response.status_code, 200, response.text)
        for url in ["/api/ai/connections", "/api/ai/routing/text", "/api/ai/routing/image"]:
            public = self.client.get(url)
            self.assertEqual(public.status_code, 200, public.text)
            self.assertNotIn(secret, public.text)
            self.assertNotIn("key_dpapi", public.text)
            self.assertNotIn("key_local", public.text)
        for config_file in self.root.glob("*-routing.json"):
            self.assertNotIn(secret, config_file.read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
