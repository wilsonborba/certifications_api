from __future__ import annotations

import asyncio
import unittest
from types import SimpleNamespace
from unittest.mock import AsyncMock, patch

from fastapi import HTTPException

from src.core.settings import app_settings
from src.presentation.routes.waitlist_route import WaitlistPayload, join_waitlist


class _Redis:
    def __init__(self) -> None:
        self.values: dict[str, object] = {}
        self.counts: dict[str, int] = {}

    def k(self, *parts: object) -> str:
        return ":".join(str(part) for part in parts)

    async def incr(self, key: str) -> int:
        self.counts[key] = self.counts.get(key, 0) + 1
        return self.counts[key]

    async def expire(self, key: str, seconds: int) -> bool:
        return True

    async def get(self, key: str):
        return self.values.get(key)

    async def set(self, key: str, value: object, **kwargs) -> bool:
        self.values[key] = value
        return True


class _Request:
    client = SimpleNamespace(host="127.0.0.1")

    def __init__(self, registered: str = "true") -> None:
        self.headers = {
            "x-certifications-service-key": app_settings().CERTIFICATIONS_SERVICE_KEY,
            "x-certifications-waitlist-registered": registered,
        }
        self.app = SimpleNamespace(state=SimpleNamespace(redis=_Redis()))


class _Fsm:
    async def upload(self, **kwargs) -> str:
        return "waitlist-object-key"


class WaitlistContractTests(unittest.TestCase):
    def test_accepts_and_deduplicates_by_email_and_plan(self) -> None:
        request = _Request()
        with patch(
            "src.presentation.routes.waitlist_route._fsm", return_value=_Fsm()
        ), patch(
            "src.presentation.routes.waitlist_route.asyncio.sleep", new=AsyncMock()
        ):
            first = asyncio.run(
                join_waitlist(WaitlistPayload(email=" Person@Example.com "), request)
            )
            second = asyncio.run(
                join_waitlist(WaitlistPayload(email="person@example.com"), request)
            )

        self.assertEqual(first, {"data": {"accepted": True}, "message": "Waitlist request accepted"})
        self.assertEqual(second, first)

    def test_rejects_missing_service_credential(self) -> None:
        request = _Request()
        request.headers["x-certifications-service-key"] = "forged"
        with self.assertRaises(HTTPException) as raised:
            asyncio.run(join_waitlist(WaitlistPayload(email="person@example.com"), request))
        self.assertEqual(raised.exception.status_code, 403)

