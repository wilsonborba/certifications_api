from __future__ import annotations

import unittest
import json

import httpx

from src.dal.remote.cortex_adapter import CortexAdapter


class CortexAdapterTests(unittest.IsolatedAsyncioTestCase):
    async def test_question_generation_forwards_raw_prompt_and_explicit_web_choice_without_client_timeout(self) -> None:
        captured: dict[str, object] = {}

        async def handler(request: httpx.Request) -> httpx.Response:
            captured["url"] = str(request.url)
            captured["payload"] = json.loads(request.content)
            return httpx.Response(
                200,
                json={
                    "request_id": "cortex-request",
                    "tier_requested": 2,
                    "tier_executed": 2,
                    "success": True,
                    "response": "{\"questions\":[]}",
                },
            )

        adapter = CortexAdapter(
            "http://127.0.0.1:8003/",
            tenant_id="certifications",
            transport=httpx.MockTransport(handler),
        )

        result = await adapter.execute_question_generation(
            prompt="contract prompt", tier=2, use_web=True
        )

        self.assertTrue(result.success)
        self.assertEqual(captured["url"], "http://127.0.0.1:8003/execute")
        payload = captured["payload"]
        self.assertIsInstance(payload, dict)
        self.assertFalse(payload["normalize_prompt"])
        self.assertTrue(payload["needs_web"])
        self.assertFalse(payload["auto_retrieval"])

    async def test_question_generation_disables_web_by_default(self) -> None:
        captured: dict[str, object] = {}

        async def handler(request: httpx.Request) -> httpx.Response:
            captured["payload"] = json.loads(request.content)
            return httpx.Response(
                200,
                json={
                    "request_id": "cortex-request",
                    "tier_requested": 0,
                    "tier_executed": 0,
                    "success": True,
                    "response": "{\"questions\":[]}",
                },
            )

        adapter = CortexAdapter(
            "http://127.0.0.1:8003",
            tenant_id="certifications",
            transport=httpx.MockTransport(handler),
        )
        await adapter.execute_question_generation(prompt="contract prompt", tier=0)
        payload = captured["payload"]
        self.assertIsInstance(payload, dict)
        self.assertFalse(payload["needs_web"])
