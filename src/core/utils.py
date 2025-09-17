
from fastapi import Request

from src.dal.local.redis_adapter import RedisAdapter


def get_redis_adapter(request: Request) -> RedisAdapter:
    adapter: RedisAdapter = request.app.state.redis
    return adapter