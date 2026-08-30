from __future__ import annotations

import asyncio
import unittest
from contextlib import contextmanager
from types import SimpleNamespace
from unittest.mock import AsyncMock, patch

from fastapi import HTTPException

from src.core.settings import app_settings
from src.presentation.routes.waitlist_route import WaitlistPayload, join_waitlist


class _FakeSession:
    """Records executed statements without touching a real database."""

    def __init__(self) -> None:
        self.executed: list[object] = []

    def execute(self, statement: object) -> None:
        self.executed.append(statement)


class _FakeDBAdapter:
    """Stands in for DBAdapter so tests never open a live Postgres session."""

    def __init__(self) -> None:
        self.session = _FakeSession()

    @contextmanager
    def session_scope(self):
        yield self.session


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
        if kwargs.get("nx") and key in self.values:
            return False
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


class WaitlistContractTests(unittest.TestCase):
    def test_accepts_and_deduplicates_by_email_and_plan(self) -> None:
        request = _Request()
        fake_db = _FakeDBAdapter()
        with (
            patch(
                "src.presentation.routes.waitlist_route.asyncio.sleep", new=AsyncMock()
            ),
            patch(
                "src.presentation.routes.waitlist_route.DBAdapter",
                return_value=fake_db,
            ),
        ):
            first = asyncio.run(
                join_waitlist(WaitlistPayload(email=" Person@Example.com "), request)
            )
            second = asyncio.run(
                join_waitlist(WaitlistPayload(email="person@example.com"), request)
            )

        self.assertEqual(first, {"data": {"accepted": True}, "message": "Waitlist request accepted"})
        self.assertEqual(second, first)
        # Both calls upserted the same normalized (email, plan) pair, never
        # touching a real database.
        self.assertEqual(len(fake_db.session.executed), 2)

    def test_rejects_missing_service_credential(self) -> None:
        request = _Request()
        request.headers["x-certifications-service-key"] = "forged"
        fake_db = _FakeDBAdapter()
        with patch(
            "src.presentation.routes.waitlist_route.DBAdapter", return_value=fake_db
        ):
            with self.assertRaises(HTTPException) as raised:
                asyncio.run(join_waitlist(WaitlistPayload(email="person@example.com"), request))
        self.assertEqual(raised.exception.status_code, 403)
        # Rejected before any DB work was attempted.
        self.assertEqual(fake_db.session.executed, [])
