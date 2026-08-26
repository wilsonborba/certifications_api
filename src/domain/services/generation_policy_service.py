from __future__ import annotations

import hashlib
from datetime import UTC, datetime
from random import Random

from src.core.settings import Settings
from src.dal.local.redis_adapter import RedisAdapter, RedisAdapterError
from src.dal.remote.cortex_adapter import CortexAdapter, CortexUnavailableError
from src.domain.models.generation_policy import (
    DEFAULT_TIER_WEIGHTS,
    GenerationOutcome,
    GenerationRequest,
    GenerationStatus,
    StudyDifficulty,
)


class GenerationPolicyService:
    """Enforces Certifications product limits before one Cortex invocation.

    It deliberately does not inspect model availability: Cortex already owns
    that responsibility. Redis locks make the app-level limits work across API
    workers and are released after each terminal result.
    """

    def __init__(self, *, redis: RedisAdapter, cortex: CortexAdapter, settings: Settings) -> None:
        self._redis = redis
        self._cortex = cortex
        self._settings = settings

    @staticmethod
    def choose_tier(request: GenerationRequest) -> int:
        weights = DEFAULT_TIER_WEIGHTS[request.difficulty]
        # Stable choice makes a retry/idempotency key select the same tier.
        seed = int(hashlib.sha256(request.idempotency_key.encode()).hexdigest(), 16)
        point = Random(seed).randrange(sum(item.weight for item in weights))
        cursor = 0
        for item in weights:
            cursor += item.weight
            if point < cursor:
                return item.tier
        return weights[-1].tier

    async def generate(self, *, user_id: str, request: GenerationRequest) -> GenerationOutcome:
        tier = self.choose_tier(request)
        group = "t0" if tier == 0 else "premium"
        user_lock = self._redis.k("generation", "user", user_id)
        global_lock = self._redis.k("generation", "global", group)
        day = datetime.now(UTC).date().isoformat()
        credit_kind = "easy" if request.difficulty is StudyDifficulty.easy else "premium"
        credit_key = self._redis.k("generation", "credits", credit_kind, user_id, day)
        credit_limit = (
            self._settings.GENERATION_EASY_DAILY_LIMIT
            if credit_kind == "easy"
            else self._settings.GENERATION_PREMIUM_DAILY_LIMIT
        )
        global_limit = (
            self._settings.GENERATION_T0_GLOBAL_CONCURRENCY
            if group == "t0"
            else self._settings.GENERATION_PREMIUM_GLOBAL_CONCURRENCY
        )
        lease = self._settings.GENERATION_LEASE_SECONDS

        try:
            if not await self._redis.acquire_lock(user_lock, lease):
                return GenerationOutcome(status=GenerationStatus.already_running, retryable=True, error_code="generation_already_running")
            # A scoped lock slot avoids relying on an unsafe shared counter.
            acquired_global = False
            for index in range(global_limit):
                if await self._redis.acquire_lock(f"{global_lock}:{index}", lease):
                    global_lock = f"{global_lock}:{index}"
                    acquired_global = True
                    break
            if not acquired_global:
                await self._redis.release_lock(user_lock)
                return GenerationOutcome(status=GenerationStatus.unavailable, retryable=True, error_code="generation_unavailable")

            used = int(await self._redis.raw.get(credit_key) or 0)
            if used >= credit_limit:
                return GenerationOutcome(status=GenerationStatus.quota_exhausted, error_code="generation_quota_exhausted")

            try:
                result = await self._cortex.execute_question_generation(
                    prompt=request.prompt,
                    tier=tier,
                    use_web=request.use_web,
                )
            except CortexUnavailableError:
                # No credit is consumed when Cortex does not accept/complete work.
                return GenerationOutcome(status=GenerationStatus.unavailable, retryable=True, error_code="generation_unavailable")

            if not result.success:
                return GenerationOutcome(status=GenerationStatus.failed, retryable=True, request_id=result.request_id, tier_requested=tier, error_code="generation_failed")

            # Credit is recorded only after a successful result.
            await self._redis.raw.incr(credit_key)
            await self._redis.raw.expire(credit_key, 60 * 60 * 48)
            return GenerationOutcome(status=GenerationStatus.ready, request_id=result.request_id, tier_requested=tier, response=result.response)
        except RedisAdapterError:
            return GenerationOutcome(status=GenerationStatus.unavailable, retryable=True, error_code="generation_unavailable")
        finally:
            # Releases are intentionally best-effort: an expired lease still
            # protects recovery from a dead worker.
            try:
                await self._redis.release_lock(user_lock)
                await self._redis.release_lock(global_lock)
            except RedisAdapterError:
                pass
