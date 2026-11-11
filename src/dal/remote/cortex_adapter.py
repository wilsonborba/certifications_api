from __future__ import annotations

from dataclasses import dataclass

import base64
import binascii

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
    """Adapter for the Cortex v2 chat and temporary file contracts."""

    def __init__(
        self,
        base_url: str,
        *,
        tenant_id: str,
        transport: httpx.AsyncBaseTransport | None = None,
    ) -> None:
        self._base_url = base_url.rstrip("/")
        self._tenant_id = tenant_id
        self._transport = transport

    async def execute_question_generation(
        self,
        *,
        prompt: str,
        tier: int,
        user_id: str,
        use_web: bool = False,
        attachments: list[dict[str, str]] | None = None,
    ) -> CortexResult:
        if not user_id:
            raise CortexUnavailableError("The generation user identity is missing")
        payload = {
            "app_id": self._tenant_id,
            "text": prompt,
            "thinking": tier >= 3,
            "needs_web": use_web,
            "web_mode": "fast",
            "use_memory": False,
            "temporary": True,
            "stream": False,
            "file_ids": [],
        }
        try:
            # Cortex owns execution deadlines and queue behaviour.  Do not
            # terminate a valid local model run from this client layer.
            async with httpx.AsyncClient(
                timeout=None, transport=self._transport, headers={"x-uuid": user_id}
            ) as client:
                for attachment in attachments or []:
                    try:
                        raw = base64.b64decode(attachment["data_base64"], validate=True)
                        filename = attachment["filename"]
                        mime_type = attachment["mime_type"]
                    except (KeyError, ValueError, binascii.Error) as exc:
                        raise CortexUnavailableError("The source attachment is invalid") from exc
                    upload = await client.post(
                        f"{self._base_url}/files",
                        data={"app_id": self._tenant_id, "temporary": "true"},
                        files={"file": (filename, raw, mime_type)},
                    )
                    if not upload.is_success:
                        raise CortexUnavailableError("Cortex could not accept the source attachment")
                    try:
                        file_id = upload.json()["data"]["file_id"]
                        if not isinstance(file_id, str) or not file_id:
                            raise ValueError("No file identifier")
                        payload["file_ids"].append(file_id)
                    except (KeyError, TypeError, ValueError) as exc:
                        raise CortexUnavailableError("Cortex did not save the source attachment") from exc
                response = await client.post(f"{self._base_url}/chats", json=payload)
        except httpx.HTTPError as exc:
            raise CortexUnavailableError("Cortex connection failed") from exc

        if response.status_code in {409, 429, 502, 503, 504}:
            raise CortexUnavailableError("Cortex is temporarily unavailable")
        if response.status_code >= 400:
            raise CortexUnavailableError("Cortex rejected the request")

        try:
            data = response.json()["data"]
            text = data["text"]
            if not isinstance(text, str):
                raise ValueError("Answer text is invalid")
            return CortexResult(
                request_id=str(data["output_id"]),
                tier_requested=tier,
                tier_executed=0,
                response=text,
                success=data["status"] == "done" and data.get("error_number") is None,
                error_type=str(data["error_number"]) if data.get("error_number") is not None else None,
            )
        except (KeyError, TypeError, ValueError) as exc:
            raise CortexUnavailableError("Cortex returned an invalid response") from exc
