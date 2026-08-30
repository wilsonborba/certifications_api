from __future__ import annotations

import asyncio
import hashlib
import math
import re
import secrets
from datetime import UTC, datetime

from fastapi import APIRouter, HTTPException, Request, status
from pydantic import BaseModel, Field
from sqlalchemy.dialects.postgresql import insert as pg_insert

from src.core.logs import error
from src.core.settings import app_settings
from src.dal.local.db_adapter import DBAdapter
from src.dal.local.orm import PlanWaitlist, User

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
    """Record one product-interest request per normalized email/plan in PostgreSQL.

    Only api_for_apps may call this route. It supplies the service credential,
    optional Supabase X-UUID and X-User-Email, and server-side registration
    annotation. Supports both logged-in and unregistered (anonymous,
    pre-login) waitlist requests: when the gateway forwards an authenticated
    session's X-User-Email, that trusted value is used for the users upsert;
    otherwise the request body's own payload.email is used instead.
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

    user_uuid = request.headers.get("x-uuid")
    trusted_email_header = request.headers.get("x-user-email")
    user_email = _normalize_email(trusted_email_header) if trusted_email_header else email
    db = DBAdapter()
    now = datetime.now(UTC)

    try:
        with db.session_scope() as session:
            # 1. Lazy provisioning of user if user_uuid is provided from Gateway.
            # The users table upsert must use the trusted X-User-Email header
            # (the authenticated session's email) rather than the request
            # body's payload.email, which the caller fully controls.
            if user_uuid:
                user_stmt = (
                    pg_insert(User)
                    .values(id=user_uuid, email=user_email, created_at=now, updated_at=now)
                    .on_conflict_do_update(
                        index_elements=["id"],
                        set_={"email": user_email, "updated_at": now},
                    )
                )
                session.execute(user_stmt)

            # 2. Upsert waitlist entry for (email, plan)
            waitlist_stmt = (
                pg_insert(PlanWaitlist)
                .values(
                    user_id=user_uuid if user_uuid else None,
                    email=email,
                    plan=payload.plan,
                    is_registered=(registered == "true"),
                    status="pending",
                    ip_address=_request_ip(request),
                    user_agent=request.headers.get("user-agent"),
                    requested_at=now,
                    updated_at=now,
                )
                .on_conflict_do_update(
                    constraint="uq_plan_waitlist_email_plan",
                    set_={
                        "user_id": user_uuid if user_uuid else PlanWaitlist.user_id,
                        "is_registered": (registered == "true"),
                        "ip_address": _request_ip(request),
                        "user_agent": request.headers.get("user-agent"),
                        "updated_at": now,
                    },
                )
            )
            session.execute(waitlist_stmt)
    except Exception as exc:
        error(f"Waitlist PostgreSQL persistence failed: {exc}")
        raise HTTPException(
            status_code=status.HTTP_503_SERVICE_UNAVAILABLE,
            detail="Waitlist is temporarily unavailable",
        ) from None

    return {"data": {"accepted": True}, "message": "Waitlist request accepted"}
