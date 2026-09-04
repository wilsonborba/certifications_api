from __future__ import annotations

from enum import StrEnum
from typing import Literal

from pydantic import BaseModel, Field


class StudyDifficulty(StrEnum):
    easy = "easy"
    medium = "medium"
    hard = "hard"


class TierWeight(BaseModel):
    tier: int = Field(ge=0, le=5)
    weight: int = Field(gt=0, le=100)


DEFAULT_TIER_WEIGHTS: dict[StudyDifficulty, tuple[TierWeight, ...]] = {
    # Basic/easy is the only difficulty exposed in the UI (#45) and is
    # promised as fast, local, single-model generation — tier 0 is the only
    # tier that guarantees that (allow_external=False, max_model_calls=1 in
    # cortex's tier envelope). No fallback weight to tier 1+: that would let
    # some chunks silently escape to slow multi-provider external fallback.
    StudyDifficulty.easy: (
        TierWeight(tier=0, weight=100),
    ),
    StudyDifficulty.medium: (
        TierWeight(tier=0, weight=45), TierWeight(tier=1, weight=25),
        TierWeight(tier=2, weight=15), TierWeight(tier=3, weight=10),
        TierWeight(tier=4, weight=4), TierWeight(tier=5, weight=1),
    ),
    StudyDifficulty.hard: (
        TierWeight(tier=0, weight=15), TierWeight(tier=1, weight=10),
        TierWeight(tier=2, weight=20), TierWeight(tier=3, weight=25),
        TierWeight(tier=4, weight=20), TierWeight(tier=5, weight=10),
    ),
}


class GenerationRequest(BaseModel):
    study_id: str = Field(min_length=1, max_length=128)
    difficulty: StudyDifficulty
    idempotency_key: str = Field(min_length=16, max_length=128)
    prompt: str = Field(min_length=1, max_length=120_000)
    use_web: bool = False
    consume_credit: bool = True


class GenerationStatus(StrEnum):
    ready = "ready"
    unavailable = "unavailable"
    quota_exhausted = "quota_exhausted"
    already_running = "already_running"
    failed = "failed"


class GenerationOutcome(BaseModel):
    status: GenerationStatus
    retryable: bool = False
    request_id: str | None = None
    tier_requested: int | None = None
    response: str | None = None
    error_code: Literal[
        "generation_unavailable", "generation_quota_exhausted",
        "generation_already_running", "generation_failed",
    ] | None = None
