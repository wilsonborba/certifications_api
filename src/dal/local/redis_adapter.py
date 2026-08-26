from __future__ import annotations
import asyncio
import msgpack
from typing import Any, Optional, Iterable, Dict, List, Tuple, Union, AsyncIterator, Callable
import redis.asyncio as aioredis

class RedisAdapterError(Exception):
    pass

class RedisAdapter:
    """
    Adapter assíncrono para Redis com:
    - Conexão única (connection pool)
    - Namespace de chaves
    - Encode/Decode (msgpack por padrão)
    - Operações CRUD comuns e utilitários de TTL/incr/locks
    - Acesso 'raw' ao cliente para comandos não cobertos
    """

    def __init__(
        self,
        redis_url: str,
        *,
        namespace: str = "",
        encode: Callable[[Any], bytes] | None = None,
        decode: Callable[[bytes], Any] | None = None,
    ) -> None:
        self._redis_url = redis_url
        self._namespace = namespace
        self._encode = encode or (lambda v: msgpack.packb(v, use_bin_type=True))
        self._decode = decode or (lambda b: msgpack.unpackb(b, raw=False))
        self._client: Optional[aioredis.Redis] = None

    # ---------- lifecycle ----------
    async def connect(self) -> None:
        if self._client is None:
            # DO NOT await from_url (it returns a client immediately)
            self._client = aioredis.from_url(
                self._redis_url,
                # IMPORTANT: use a real encoding so str keys/args are encoded correctly
                encoding="utf-8",
                decode_responses=False,   # keep bytes responses; you handle decode yourself
                health_check_interval=30, # optional but recommended
            )

    async def close(self) -> None:
        if self._client is not None:
            await self._client.close()
            self._client = None

    @property
    def raw(self) -> aioredis.Redis:
        """Acesso ao cliente Redis bruto para comandos especiais."""
        if self._client is None:
            raise RedisAdapterError("Redis client not connected. Call connect() first.")
        return self._client

    # ---------- keys & namespace ----------
    def k(self, *parts: Union[str, int]) -> str:
        """Monta chave com namespace: namespace + ':'.join(parts)"""
        base = ":".join(str(p) for p in parts if p is not None and str(p) != "")
        return f"{self._namespace}{base}"

    # ---------- encode/decode helpers ----------
    def _maybe_encode(self, value: Any) -> bytes:
        if isinstance(value, (bytes, bytearray, memoryview)):
            return bytes(value)
        return self._encode(value)

    def _maybe_decode(self, value: Optional[bytes]) -> Any:
        if value is None:
            return None
        return self._decode(value)

    # ---------- basic KV ----------
    async def set(self, key: str, value: Any, ex: Optional[int] = None, nx: bool = False) -> bool:
        """
        ex: TTL em segundos, nx=True => SET if Not eXists
        """
        try:
            res = await self.raw.set(key, self._maybe_encode(value), ex=ex, nx=nx)
            return bool(res)
        except Exception as e:
            raise RedisAdapterError(str(e)) from e

    async def get(self, key: str) -> Any:
        try:
            val = await self.raw.get(key)
            return self._maybe_decode(val)
        except Exception as e:
            raise RedisAdapterError(str(e)) from e

    async def delete(self, *keys: str) -> int:
        try:
            return int(await self.raw.delete(*keys))
        except Exception as e:
            raise RedisAdapterError(str(e)) from e

    async def exists(self, *keys: str) -> int:
        try:
            return int(await self.raw.exists(*keys))
        except Exception as e:
            raise RedisAdapterError(str(e)) from e

    async def expire(self, key: str, seconds: int) -> bool:
        try:
            return bool(await self.raw.expire(key, seconds))
        except Exception as e:
            raise RedisAdapterError(str(e)) from e

    async def ttl(self, key: str) -> int:
        try:
            return int(await self.raw.ttl(key))
        except Exception as e:
            raise RedisAdapterError(str(e)) from e

    async def incr(self, key: str, amount: int = 1) -> int:
        try:
            return int(await self.raw.incrby(key, amount))
        except Exception as e:
            raise RedisAdapterError(str(e)) from e

    async def mget(self, keys: Iterable[str]) -> List[Any]:
        try:
            vals = await self.raw.mget(list(keys))
            return [self._maybe_decode(v) if v is not None else None for v in vals]
        except Exception as e:
            raise RedisAdapterError(str(e)) from e

    async def mset(self, mapping: Dict[str, Any]) -> bool:
        try:
            encoded = {k: self._maybe_encode(v) for k, v in mapping.items()}
            return bool(await self.raw.mset(encoded))
        except Exception as e:
            raise RedisAdapterError(str(e)) from e

    # ---------- hash ----------
    async def hset(self, key: str, mapping: Dict[str, Any]) -> int:
        try:
            enc = {f: self._maybe_encode(v) for f, v in mapping.items()}
            return int(await self.raw.hset(key, mapping=enc))
        except Exception as e:
            raise RedisAdapterError(str(e)) from e

    async def hget(self, key: str, field: str) -> Any:
        try:
            v = await self.raw.hget(key, field)
            return self._maybe_decode(v)
        except Exception as e:
            raise RedisAdapterError(str(e)) from e

    async def hgetall(self, key: str) -> Dict[str, Any]:
        try:
            res = await self.raw.hgetall(key)
            # res vem como dict[str, bytes]
            return {k.decode(): self._maybe_decode(v) for k, v in res.items()}
        except Exception as e:
            raise RedisAdapterError(str(e)) from e

    # ---------- sets ----------
    async def sadd(self, key: str, *members: Any) -> int:
        try:
            enc = [self._maybe_encode(m) for m in members]
            return int(await self.raw.sadd(key, *enc))
        except Exception as e:
            raise RedisAdapterError(str(e)) from e

    async def smembers(self, key: str) -> List[Any]:
        try:
            vals = await self.raw.smembers(key)
            return [self._maybe_decode(v) for v in vals]
        except Exception as e:
            raise RedisAdapterError(str(e)) from e

    async def sismember(self, key: str, member: Any) -> bool:
        try:
            return bool(await self.raw.sismember(key, self._maybe_encode(member)))
        except Exception as e:
            raise RedisAdapterError(str(e)) from e

    async def srem(self, key: str, *members: Any) -> int:
        try:
            return int(await self.raw.srem(key, *(self._maybe_encode(member) for member in members)))
        except Exception as e:
            raise RedisAdapterError(str(e)) from e

    # ---------- sorted sets ----------
    async def zadd(self, key: str, *score_member: Tuple[float, Any]) -> int:
        try:
            mapping = {self._maybe_encode(m): s for s, m in score_member}
            return int(await self.raw.zadd(key, mapping))
        except Exception as e:
            raise RedisAdapterError(str(e)) from e

    async def zrange(self, key: str, start: int, end: int, withscores: bool = False) -> List[Any]:
        try:
            vals = await self.raw.zrange(key, start, end, withscores=withscores)
            if withscores:
                # [(b'value', score), ...]
                return [(self._maybe_decode(v), s) for v, s in vals]
            return [self._maybe_decode(v) for v in vals]
        except Exception as e:
            raise RedisAdapterError(str(e)) from e

    # ---------- pubsub (básico) ----------
    async def publish(self, channel: str, message: Any) -> int:
        try:
            return int(await self.raw.publish(channel, self._maybe_encode(message)))
        except Exception as e:
            raise RedisAdapterError(str(e)) from e

    async def subscribe(self, *channels: str) -> aioredis.client.PubSub:
        try:
            ps = self.raw.pubsub()
            await ps.subscribe(*channels)
            return ps
        except Exception as e:
            raise RedisAdapterError(str(e)) from e

    # ---------- locks simples ----------
    async def acquire_lock(self, key: str, ttl_sec: int) -> bool:
        """
        Lock simples com SET NX EX.
        """
        try:
            return await self.set(key, "1", ex=ttl_sec, nx=True)
        except Exception as e:
            raise RedisAdapterError(str(e)) from e

    async def release_lock(self, key: str) -> int:
        try:
            return await self.delete(key)
        except Exception as e:
            raise RedisAdapterError(str(e)) from e
