from __future__ import annotations

import asyncio
import unittest

from redis.exceptions import ConnectionError as RedisConnectionError

from main import redis_unavailable
from src.dal.local.redis_adapter import RedisAdapter, RedisAdapterError


class _UnavailableRedis:
    async def ping(self) -> bool:
        raise RedisConnectionError("connection details must not reach clients")


class RedisResilienceTests(unittest.TestCase):
    def test_ping_normalizes_redis_connection_errors(self) -> None:
        adapter = RedisAdapter("redis://unused")
        adapter._client = _UnavailableRedis()  # type: ignore[assignment]

        with self.assertRaises(RedisAdapterError):
            asyncio.run(adapter.ping())

    def test_redis_error_response_is_generic(self) -> None:
        response = asyncio.run(redis_unavailable(None, RedisConnectionError("internal host:port")))  # type: ignore[arg-type]

        self.assertEqual(response.status_code, 503)
        self.assertNotIn(b"internal host:port", response.body)
        self.assertEqual(
            response.body,
            b'{"detail":"Study service is temporarily unavailable. Please try again shortly."}',
        )
