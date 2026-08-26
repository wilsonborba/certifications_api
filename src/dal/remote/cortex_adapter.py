from __future__ import annotations

from dataclasses import dataclass

import httpx


class CortexUnavailableError(RuntimeError):
    """Cortex could not accept or complete this request safely."""


@dataclass(frozen=True)
class CortexResult:
    request_id: str
    tier_requested: int
    tier_executed: int
    response: str
    success: bool
    error_type: str | None = None


class CortexAdapter:
    """Small, explicit adapter around Cortex's documented ``POST /execute`` API."""

    def __init__(self, base_url: str, *, timeout_seconds: float, tenant_id: str) -> None:
        self._base_url = base_url.rstrip("/")
        self._timeout = timeout_seconds
        self._tenant_id = tenant_id

    async def execute_question_generation(self, *, prompt: str, tier: int) -> CortexResult:
        payload = {
            "prompt": prompt,
            "tenant_id": self._tenant_id,
            "tier": tier,
            "task_type": "education_question_generation",
            "normalize_prompt": True,
            "thinking": tier >= 3,
            "needs_web": False,
            "use_memory": False,
            "auto_retrieval": False,
        }
        try:
            async with httpx.AsyncClient(timeout=self._timeout) as client:
                response = await client.post(f"{self._base_url}/execute", json=payload)
        except httpx.HTTPError as exc:
            raise CortexUnavailableError("Cortex connection failed") from exc

        if response.status_code in {409, 429, 502, 503, 504}:
            raise CortexUnavailableError("Cortex is temporarily unavailable")
        if response.status_code >= 400:
            raise CortexUnavailableError("Cortex rejected the request")

        try:
            data = response.json()
            return CortexResult(
                request_id=str(data["request_id"]),
                tier_requested=int(data["tier_requested"]),
                tier_executed=int(data["tier_executed"]),
                response=str(data.get("response", "")),
                success=bool(data.get("success")),
                error_type=data.get("error_type"),
            )
        except (KeyError, TypeError, ValueError) as exc:
            raise CortexUnavailableError("Cortex returned an invalid response") from exc
