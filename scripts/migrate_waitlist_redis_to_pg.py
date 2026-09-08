#!/usr/bin/env python3
"""Script to backfill legacy waitlist records stored in Redis into PostgreSQL.

After each record is successfully committed to PostgreSQL, its Redis key is
deleted so Redis genuinely returns to being ephemeral-only, per the
architecture doc. A key is never deleted if the PostgreSQL write for it did
not succeed.

Usage:
    python scripts/migrate_waitlist_redis_to_pg.py
"""

import asyncio
from datetime import UTC, datetime

from sqlalchemy.dialects.postgresql import insert as pg_insert

from src.core.logs import error, info
from src.core.settings import app_settings
from src.dal.local.db_adapter import DBAdapter
from src.dal.local.orm import PlanWaitlist
from src.dal.local.redis_adapter import RedisAdapter


async def backfill() -> None:
    settings = app_settings()
    redis = RedisAdapter(settings.REDIS_URL, namespace=settings.REDIS_NAMESPACE)
    await redis.connect()

    db = DBAdapter()
    pattern = redis.k("waitlist", "*")
    keys = await redis.raw.keys(pattern)
    info(f"Found {len(keys)} legacy waitlist keys in Redis.")

    migrated_count = 0
    deleted_count = 0
    now = datetime.now(UTC)

    for key in keys:
        # `key` is already the fully-qualified raw Redis key returned by
        # `raw.keys(...)` (namespace included). RedisAdapter.get()/delete()
        # operate on raw keys as-is; they do not re-apply the namespace, so
        # the raw key must be used unchanged for those calls. `clean_key`
        # (namespace stripped) is kept only for readable logging: the
        # namespace prefix is normalized with `rstrip(':')` before the strip
        # so this works whether or not REDIS_NAMESPACE itself ends in ':'.
        key_str = key.decode("utf-8") if isinstance(key, bytes) else key
        namespace_prefix = f"{settings.REDIS_NAMESPACE.rstrip(':')}:" if settings.REDIS_NAMESPACE else ""
        clean_key = key_str.replace(namespace_prefix, "", 1) if namespace_prefix else key_str

        raw_data = await redis.get(key_str)
        if not raw_data or not isinstance(raw_data, dict):
            continue

        email = raw_data.get("email")
        plan = raw_data.get("plan", "free")
        is_registered = raw_data.get("is_registered", False)
        requested_at_str = raw_data.get("requested_at")
        requested_at = datetime.fromisoformat(requested_at_str) if requested_at_str else now

        if not email:
            continue

        stmt = (
            pg_insert(PlanWaitlist)
            .values(
                email=email,
                plan=plan,
                is_registered=is_registered,
                status="pending",
                requested_at=requested_at,
                updated_at=now,
            )
            .on_conflict_do_update(
                constraint="uq_plan_waitlist_email_plan",
                set_={
                    "is_registered": is_registered,
                    "updated_at": now,
                },
            )
        )

        try:
            with db.session_scope() as session:
                session.execute(stmt)
        except Exception as exc:
            error(f"Failed to migrate waitlist key {clean_key}: {exc}")
            continue

        migrated_count += 1

        # Only delete the Redis key once the PostgreSQL write for it has
        # actually committed (the `with` block above exited without error).
        await redis.delete(key_str)
        deleted_count += 1

    await redis.close()
    info(
        f"Successfully migrated {migrated_count} waitlist records to PostgreSQL "
        f"and deleted {deleted_count} legacy Redis keys."
    )


if __name__ == "__main__":
    asyncio.run(backfill())
