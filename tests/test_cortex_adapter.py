from __future__ import annotations

import base64
import json
import unittest

import httpx

from src.dal.remote.cortex_adapter import CortexAdapter, CortexUnavailableError


class CortexAdapterTests(unittest.IsolatedAsyncioTestCase):
    @staticmethod
    def reply() -> dict:
        return {"message": "Chat created", "data": {"output_id": "output-1", "status": "done", "text": "answer"}}

    async def test_v2_contract_forwards_identity_and_keeps_security_enabled(self) -> None:
        async def handler(request: httpx.Request) -> httpx.Response:
            self.assertEqual(request.url.path, "/chats")
            self.assertEqual(request.headers["x-uuid"], "user-1")
            self.assertEqual(request.extensions["timeout"]["read"], None)
            self.assertEqual(json.loads(request.content), {
                "app_id": "certifications", "text": "contract prompt", "thinking": False,
                "needs_web": True, "web_mode": "fast", "use_memory": False,
                "temporary": True, "stream": False, "file_ids": [],
            })
            return httpx.Response(201, json=self.reply())
        adapter = CortexAdapter("http://127.0.0.1:8003/", tenant_id="certifications", transport=httpx.MockTransport(handler))
        result = await adapter.execute_question_generation(prompt="contract prompt", tier=2, user_id="user-1", use_web=True)
        self.assertTrue(result.success)
        self.assertEqual(result.request_id, "output-1")
        self.assertEqual(result.response, "answer")
        self.assertEqual(result.tier_requested, 2)
        self.assertEqual(result.tier_executed, 0)

    async def test_web_defaults_off_and_thinking_choice_is_preserved(self) -> None:
        async def handler(request: httpx.Request) -> httpx.Response:
            body = json.loads(request.content)
            self.assertFalse(body["needs_web"])
            self.assertTrue(body["thinking"])
            return httpx.Response(201, json=self.reply())
        adapter = CortexAdapter("http://cortex", tenant_id="certifications", transport=httpx.MockTransport(handler))
        await adapter.execute_question_generation(prompt="contract prompt", tier=3, user_id="user-1")

    async def test_audio_is_uploaded_temporarily_before_chat_with_same_identity(self) -> None:
        paths = []
        async def handler(request: httpx.Request) -> httpx.Response:
            paths.append(request.url.path)
            self.assertEqual(request.headers["x-uuid"], "user-1")
            if request.url.path == "/files":
                body = request.content
                self.assertIn(b'name="temporary"\r\n\r\ntrue', body)
                self.assertIn(b'name="app_id"\r\n\r\ncertifications', body)
                self.assertIn(b'filename="selected-audio.wav"', body)
                self.assertIn(b"audio/wav", body)
                self.assertIn(b"synthetic audio", body)
                return httpx.Response(201, json={"data": {"file_id": "file-1"}})
            self.assertEqual(json.loads(request.content)["file_ids"], ["file-1"])
            return httpx.Response(201, json=self.reply())
        adapter = CortexAdapter("http://cortex", tenant_id="certifications", transport=httpx.MockTransport(handler))
        await adapter.execute_question_generation(prompt="transcribe", tier=0, user_id="user-1", attachments=[{
            "filename": "selected-audio.wav", "mime_type": "audio/wav",
            "data_base64": base64.b64encode(b"synthetic audio").decode(),
        }])
        self.assertEqual(paths, ["/files", "/chats"])

    async def test_missing_identity_and_missing_upload_identifier_fail_safely(self) -> None:
        adapter = CortexAdapter("http://cortex", tenant_id="certifications", transport=httpx.MockTransport(lambda _: httpx.Response(200, json={"data": {}})))
        with self.assertRaises(CortexUnavailableError):
            await adapter.execute_question_generation(prompt="text", tier=0, user_id="")
        with self.assertRaises(CortexUnavailableError):
            await adapter.execute_question_generation(prompt="text", tier=0, user_id="user-1", attachments=[{
                "filename": "file.wav", "mime_type": "audio/wav", "data_base64": "YQ==",
            }])

    async def test_error_status_invalid_response_and_failed_output_are_not_success(self) -> None:
        for status, body in [(400, {}), (503, {}), (200, {"data": {"output_id": "x"}})]:
            with self.subTest(status=status):
                adapter = CortexAdapter("http://cortex", tenant_id="certifications", transport=httpx.MockTransport(lambda _, s=status, b=body: httpx.Response(s, json=b)))
                with self.assertRaises(CortexUnavailableError):
                    await adapter.execute_question_generation(prompt="text", tier=0, user_id="user-1")
        adapter = CortexAdapter("http://cortex", tenant_id="certifications", transport=httpx.MockTransport(lambda _: httpx.Response(201, json={"data": {"output_id": "x", "status": "failed", "text": "", "error_number": 1012}})))
        result = await adapter.execute_question_generation(prompt="text", tier=0, user_id="user-1")
        self.assertFalse(result.success)
        self.assertEqual(result.error_type, "1012")
