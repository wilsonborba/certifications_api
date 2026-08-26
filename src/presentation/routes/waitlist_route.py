from __future__ import annotations

import hashlib
import json
from datetime import UTC, datetime

from fastapi import APIRouter, HTTPException, Request, status
from pydantic import BaseModel, Field

from src.core.logs import error
from src.dal.local.redis_adapter import RedisAdapterError
from src.dal.remote.fsm_media_adapter import FsmConfigurationError, FsmStorageError
from src.presentation.routes.study_route import _fsm, _owner_id

waitlist_router = APIRouter(prefix="/waitlist")


class WaitlistPayload(BaseModel):
    plan: str = Field(default="free", pattern="^free$")


@waitlist_router.post("", status_code=status.HTTP_201_CREATED)
async def join_waitlist(payload: WaitlistPayload, request: Request) -> dict:
    """Record one authenticated product-interest request per plan/user.

    The source of truth is a private FSM JSON record. Redis is used solely as
    a no-TTL idempotency index so repeat presses do not create duplicate media
    records while the current pre-migration architecture is in place.
    """

    owner_id = _owner_id(request)
    index_key = request.app.state.redis.k("waitlist", payload.plan, owner_id)
    try:
        existing = await request.app.state.redis.raw.get(index_key)
        if existing:
            return {"data": {"already_joined": True}, "message": "Waitlist request already recorded"}

        digest = hashlib.sha256(owner_id.encode("utf-8")).hexdigest()
        body = json.dumps(
            {
                "owner_id": owner_id,
                "plan": payload.plan,
                "requested_at": datetime.now(UTC).isoformat(),
            },
            separators=(",", ":"),
        ).encode("utf-8")
        object_key = await _fsm().upload(
            album="waitlist",
            filename=f"{payload.plan}-{digest}.json",
            body=body,
            content_type="application/json",
        )
        await request.app.state.redis.raw.set(index_key, object_key)
    except (FsmConfigurationError, FsmStorageError, RedisAdapterError) as exc:
        error(f"Waitlist recording failed: {exc}")
        raise HTTPException(
            status_code=status.HTTP_503_SERVICE_UNAVAILABLE,
            detail="Waitlist is temporarily unavailable",
        ) from None
    return {"data": {"already_joined": False}, "message": "Waitlist request recorded"}
