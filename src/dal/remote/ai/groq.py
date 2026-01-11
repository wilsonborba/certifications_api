from __future__ import annotations

import asyncio
import json
import time
from typing import Any, Dict, List, Optional

import httpx

from src.core.settings import app_settings
from src.domain.services.ai_client_base import AiClientBase


class GroqError(RuntimeError):
    """Raised when the Groq API returns an error response."""

    def __init__(self, status_code: int, payload: Dict[str, Any] | None):
        self.status_code = status_code
        self.payload = payload or {}
        super().__init__(f"Groq API error {status_code}: {self.payload}")


class GroqConfig:
    """
    Runtime configuration for the Groq adapter.

    Base URL for OpenAI-compatible endpoints:
      https://api.groq.com/openai/v1
    """

    def __init__(
        self,
        api_key: Optional[str] = None,
        model: str = "llama-3.1-8b-instant",
        base_url: str = "https://api.groq.com/openai/v1",
        timeout: float = 3000,
    ) -> None:
        self.api_key = api_key
        self.model = model
        self.base_url = base_url.rstrip("/")
        self.timeout = timeout


class GroqClient(AiClientBase):
    """
    Async client for Groq's OpenAI-compatible API.

    Uses:
      - POST /chat/completions

    Notes:
      - Auth is Authorization: Bearer <key>
      - 429 may include Retry-After; honor it where present.
    """

    def __init__(
        self,
        cfg: Optional[GroqConfig] = None,
        client: Optional[httpx.AsyncClient] = None,
    ) -> None:
        self.cfg = cfg or GroqConfig()
        self._own_client = client is None
        self._client = client or httpx.AsyncClient(timeout=self.cfg.timeout)

        self._last_status_code: int | None = None
        self._last_attempts: int = 0
        self._last_latency_ms: float = 0.0

    def set_api_key(self, api_key: str):
        self.cfg.api_key = api_key

    @property
    def last_status_code(self) -> int | None:
        return self._last_status_code

    @property
    def last_attempts(self) -> int:
        return self._last_attempts

    @property
    def last_latency_ms(self) -> float:
        return self._last_latency_ms

    async def aclose(self) -> None:
        if self._own_client:
            await self._client.aclose()

    # --------------- public API ---------------

    async def generate_text(  # pyright: ignore[reportIncompatibleMethodOverride]
        self,
        prompt: str,
        system_instruction: Optional[str] = None,
        temperature: Optional[float] = None,
        top_p: Optional[float] = None,
        max_tokens: Optional[int] = None,
        seed: Optional[int] = None,
        # Structured outputs (optional): pass a JSON Schema dict to enforce shape
        json_schema: Optional[Dict[str, Any]] = None,
        # Or best-effort JSON without schema:
        json_object: bool = False,
        model: Optional[str] = None,
        response_mime_type: Optional[str] = None,
        *args: Any,
        **kwargs: Any,
    ) -> Dict[str, Any]:
        """
        Chat Completions call. Returns raw Groq response (dict).

        Typical extraction:
          text = resp["choices"][0]["message"]["content"]

        For structured outputs:
          - json_schema: uses response_format.type="json_schema"
          - json_object=True: uses response_format.type="json_object"
        """
        url = f"{self.cfg.base_url}/chat/completions"

        messages: List[Dict[str, Any]] = []
        if system_instruction:
            messages.append({"role": "system", "content": system_instruction})
        messages.append({"role": "user", "content": prompt})

        body: Dict[str, Any] = {
            "model": model or self.cfg.model,
            "messages": messages,
        }
        if temperature is not None:
            body["temperature"] = temperature
        if top_p is not None:
            body["top_p"] = top_p
        if max_tokens is not None:
            body["max_tokens"] = max_tokens
        if seed is not None:
            body["seed"] = seed

        # Structured outputs (Groq supports response_format modes)
        if json_schema is not None:
            body["response_format"] = {
                "type": "json_schema",
                "json_schema": {
                    "name": "response",
                    # strict=False is “best-effort”; set True if you want hard validation
                    "strict": True,
                    "schema": json_schema,
                },
            }
        elif json_object:
            body["response_format"] = {"type": "json_object"}

        return await self._request_json("POST", url, json=body)

    # --------------- HTTP + retries ---------------

    def _auth_headers(self) -> Dict[str, str]:
        return {
            "Authorization": f"Bearer {self.cfg.api_key}",
            "Content-Type": "application/json",
        }

    async def _request(
        self,
        method: str,
        url: str,
        *,
        json: Any = None,
        content: Any = None,
        max_retries: int = 3,
    ) -> httpx.Response:
        attempt = 0
        t0 = time.perf_counter()

        while True:
            attempt += 1
            resp = await self._client.request(
                method,
                url,
                headers=self._auth_headers(),
                json=json,
                content=content,
            )

            if resp.status_code < 400:
                self._last_status_code = resp.status_code
                self._last_attempts = attempt
                self._last_latency_ms = (time.perf_counter() - t0) * 1000.0
                return resp

            # Retry on transient / rate limits
            if resp.status_code in (429, 500, 502, 503, 504) and attempt <= max_retries:
                retry_after = resp.headers.get("retry-after")
                if retry_after:
                    try:
                        delay = float(retry_after)
                    except ValueError:
                        delay = min(2.0 * attempt, 10.0)
                else:
                    delay = min(2.0 * attempt, 10.0)

                await asyncio.sleep(delay)
                continue

            # failure path
            self._last_status_code = resp.status_code
            self._last_attempts = attempt
            self._last_latency_ms = (time.perf_counter() - t0) * 1000.0

            try:
                payload = resp.json()
            except Exception:
                payload = {"message": resp.text}

            raise GroqError(resp.status_code, payload)

    async def _request_json(
        self,
        method: str,
        url: str,
        *,
        json: Any = None,
        content: Any = None,
    ) -> Dict[str, Any]:
        r = await self._request(method, url, json=json, content=content)
        try:
            return r.json()
        except Exception:
            raise GroqError(r.status_code, {"message": "Invalid JSON in response"})
