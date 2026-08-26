from __future__ import annotations

import hashlib
import math
import re
import secrets
import asyncio
from datetime import UTC, datetime

from fastapi import APIRouter, HTTPException, Request, status
from pydantic import BaseModel, Field

from src.core.logs import error
from src.core.settings import app_settings
from src.dal.local.redis_adapter import RedisAdapterError

waitlist_router = APIRouter(prefix="/waitlist")


class WaitlistPayload(BaseModel):
    email: str = Field(min_length=3, max_length=320)
    plan: str = Field(default="free", pattern="^free$")


def _normalize_email(email: str) -> str:
    normalized = email.strip().lower()
    if not re.fullmatch(r"[^@\s]+@[^@\s]+\.[^@\s]+", normalized):
        raise HTTPException(status_code=422, detail="Invalid waitlist request")
    return normalized


def _request_ip(request: Request) -> str:
    forwarded = request.headers.get("x-forwarded-for")
    return (forwarded.split(",", 1)[0].strip() if forwarded else None) or (
        request.client.host if request.client else "unknown"
    )


async def _rate_limit(request: Request, email: str) -> None:
    """Defense-in-depth limits for callers that bypass the gateway."""
    redis = request.app.state.redis
    email_hash = hashlib.sha256(email.encode("utf-8")).hexdigest()
    dimensions = (
        (f"waitlist:ip:{_request_ip(request)}", ((5, 60), (20, 3600))),
        (f"waitlist:email:{email_hash}", ((3, 60), (10, 600))),
    )
    largest_count = 0
    for suffix, limits in dimensions:
        for maximum, window in limits:
            key = redis.k("rate", suffix, f"{window}s")
            count = await redis.incr(key)
            if count == 1:
                await redis.expire(key, window)
            largest_count = max(largest_count, count)
            if count > maximum:
                raise HTTPException(status_code=429, detail="Waitlist temporarily limited")
    await asyncio.sleep(min(1000, int(150 * math.log2(largest_count + 1))) / 1000)


@waitlist_router.post("", status_code=status.HTTP_202_ACCEPTED)
async def join_waitlist(payload: WaitlistPayload, request: Request) -> dict:
    """Record one public product-interest request per normalized email/plan.

    Only api_for_apps may call this route. It supplies the service credential
    and the server-side Supabase registration annotation. Waitlist metadata is
    kept in Redis for this MVP; FSM remains exclusively file storage.
    """
    settings = app_settings()
    provided_key = request.headers.get("x-certifications-service-key", "")
    if not provided_key or not secrets.compare_digest(
        provided_key, settings.CERTIFICATIONS_SERVICE_KEY
    ):
        raise HTTPException(status_code=403, detail="Waitlist authentication required")

    email = _normalize_email(payload.email)
    await _rate_limit(request, email)
    registered = request.headers.get("x-certifications-waitlist-registered")
    if registered not in {"true", "false"}:
        raise HTTPException(status_code=400, detail="Invalid waitlist context")

    digest = hashlib.sha256(f"{payload.plan}:{email}".encode("utf-8")).hexdigest()
    record_key = request.app.state.redis.k("waitlist", payload.plan, digest)
    try:
        await request.app.state.redis.set(
            record_key,
            {
                "email": email,
                "plan": payload.plan,
                "is_registered": registered == "true",
                "requested_at": datetime.now(UTC).isoformat(),
            },
            nx=True,
        )
    except RedisAdapterError as exc:
        error(f"Waitlist recording failed: {exc}")
        raise HTTPException(
            status_code=status.HTTP_503_SERVICE_UNAVAILABLE,
            detail="Waitlist is temporarily unavailable",
        ) from None
    return {"data": {"accepted": True}, "message": "Waitlist request accepted"}
