#!/usr/bin/env python3
"""Script to backfill legacy waitlist records stored in Redis into PostgreSQL.

Usage:
    python scripts/migrate_waitlist_redis_to_pg.py
"""

import asyncio
from datetime import UTC, datetime

from sqlalchemy.dialects.postgresql import insert as pg_insert

from src.core.logs import info
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
    now = datetime.now(UTC)

    with db.session_scope() as session:
        for key in keys:
            # Strip namespace prefix if raw key has it
            key_str = key.decode("utf-8") if isinstance(key, bytes) else key
            clean_key = key_str.replace(f"{settings.REDIS_NAMESPACE}:", "", 1) if settings.REDIS_NAMESPACE else key_str
            raw_data = await redis.get(clean_key)
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
            session.execute(stmt)
            migrated_count += 1

    await redis.close()
    info(f"Successfully migrated {migrated_count} waitlist records to PostgreSQL.")


if __name__ == "__main__":
    asyncio.run(backfill())
