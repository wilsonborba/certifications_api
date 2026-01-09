from __future__ import annotations

import asyncio
import json
import time
from typing import Any, Dict, Optional, Sequence, Tuple

import httpx

from src.core.settings import app_settings
from src.domain.services.ai_client_base import AiClientBase


class GeminiError(RuntimeError):
    """Raised when the Gemini API returns an error response."""

    def __init__(self, status_code: int, payload: Dict[str, Any] | None):
        self.status_code = status_code
        self.payload = payload or {}
        super().__init__(f"Gemini API error {status_code}: {self.payload}")


class GeminiConfig:
    """
    Runtime configuration for the Gemini adapter.
    - api_key is read from your app settings by default.
    - model defaults to a fast, cheap model good for MVPs.
    """

    def __init__(
        self,
        api_key: Optional[str] = None,
        model: str = "gemini-2.5-flash",  # adjust if you need Pro
        base_url: str = "https://generativelanguage.googleapis.com",
        timeout: float = 3000,
    ) -> None:
        self.api_key = api_key or app_settings().GEMINI_API_KEY
        if not self.api_key:
            raise ValueError("GEMINI_API_KEY is not set in app settings.")
        self.model = model
        self.base_url = base_url.rstrip("/")
        self.timeout = timeout


class GeminiClient(AiClientBase):
    """
    Async client for Gemini API (AI Studio). Uses:
      - generateContent for text-only prompts
      - upload/v1beta/files for file uploads (resumable)
      - generateContent with file_data parts for prompts+attachments

    Key ideas:
      - All headers/payload shapes are hidden inside.
      - Respects 429/5xx with limited retries and Retry-After.
      - response_mime_type='application/json' forces structured JSON output.
    """

    def __init__(
        self,
        cfg: Optional[GeminiConfig] = None,
        client: Optional[httpx.AsyncClient] = None,
    ) -> None:
        self.cfg = cfg or GeminiConfig()
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

    # --------------- public API ---------------

    async def aclose(self) -> None:
        if self._own_client:
            await self._client.aclose()

    async def generate_text(  # pyright: ignore[reportIncompatibleMethodOverride]
        self,
        prompt: str,
        *,
        system_instruction: Optional[str] = None,
        response_mime_type: Optional[str] = None,  # e.g. "application/json"
        temperature: Optional[float] = None,
        top_p: Optional[float] = None,
        top_k: Optional[int] = None,
        candidate_count: Optional[int] = None,
        extra_generation_config: Optional[Dict[str, Any]] = None,
        model: Optional[str] = None,
    ) -> Dict[str, Any]:
        """
        Text-only call. Returns the raw Gemini response (dict).
        Typical extraction:
            text = resp['candidates'][0]['content']['parts'][0]['text']
        If you requested JSON (response_mime_type='application/json'), parse that string.
        """
        body = self._build_generate_body(
            prompt=prompt,
            system_instruction=system_instruction,
            response_mime_type=response_mime_type,
            temperature=temperature,
            top_p=top_p,
            top_k=top_k,
            candidate_count=candidate_count,
            extra_generation_config=extra_generation_config,
            file_parts=None,
        )
        url = f"{self.cfg.base_url}/v1beta/models/{model or self.cfg.model}:generateContent"
        return await self._post_json(url, body)

    async def generate_with_attachment(
        self,
        prompt: str,
        *,
        file_bytes: bytes,
        display_name: str,
        mime_type: str = "text/csv",
        system_instruction: Optional[str] = None,
        response_mime_type: Optional[str] = None,
        temperature: Optional[float] = None,
        top_p: Optional[float] = None,
        top_k: Optional[int] = None,
        candidate_count: Optional[int] = None,
        extra_generation_config: Optional[Dict[str, Any]] = None,
        model: Optional[str] = None,
        auto_delete_file: bool = True,
    ) -> Dict[str, Any]:
        """
        Uploads a file via the Files API (resumable), then calls generateContent
        referencing the file by file_uri. Optionally deletes the file afterwards.
        """
        file_uri = await self._upload_file_resumable(
            file_bytes, display_name, mime_type
        )
        try:
            file_parts = [{"file_data": {"mime_type": mime_type, "file_uri": file_uri}}]
            body = self._build_generate_body(
                prompt=prompt,
                system_instruction=system_instruction,
                response_mime_type=response_mime_type,
                temperature=temperature,
                top_p=top_p,
                top_k=top_k,
                candidate_count=candidate_count,
                extra_generation_config=extra_generation_config,
                file_parts=file_parts,
            )
            url = f"{self.cfg.base_url}/v1beta/models/{model or self.cfg.model}:generateContent"
            return await self._post_json(url, body)
        finally:
            if auto_delete_file:
                try:
                    await self._delete_file_by_uri(file_uri)
                except Exception:
                    # Log and continue — file will auto-expire anyway per Gemini Files policy.
                    pass

    # --------------- internals ---------------

    def _auth_headers(self) -> Dict[str, str]:
        # generateContent expects API key in querystring; Files API uses header.
        return {
            "x-goog-api-key": self.cfg.api_key,
        }

    def _build_generate_body(
        self,
        *,
        prompt: str,
        system_instruction: Optional[str],
        response_mime_type: Optional[str],
        temperature: Optional[float],
        top_p: Optional[float],
        top_k: Optional[int],
        candidate_count: Optional[int],
        extra_generation_config: Optional[Dict[str, Any]],
        file_parts: Optional[Sequence[Dict[str, Any]]] = None,
    ) -> Dict[str, Any]:
        parts = []
        if prompt:
            parts.append({"text": prompt})
        if file_parts:
            parts.extend(file_parts)

        contents = [{"role": "user", "parts": parts}]
        body: Dict[str, Any] = {"contents": contents}

        gen_cfg: Dict[str, Any] = {}
        if response_mime_type:
            gen_cfg["responseMimeType"] = response_mime_type
        if temperature is not None:
            gen_cfg["temperature"] = temperature
        if top_p is not None:
            gen_cfg["topP"] = top_p
        if top_k is not None:
            gen_cfg["topK"] = top_k
        if candidate_count is not None:
            gen_cfg["candidateCount"] = candidate_count
        if extra_generation_config:
            gen_cfg.update(extra_generation_config)
        if gen_cfg:
            body["generationConfig"] = gen_cfg

        if system_instruction:
            body["systemInstruction"] = {
                "role": "system",
                "parts": [{"text": system_instruction}],
            }

        return body

    async def _post_json(self, url: str, body: Dict[str, Any]) -> Dict[str, Any]:
        # For generateContent, the API key is passed as a query param.
        params = {"key": self.cfg.api_key}
        headers = {"Content-Type": "application/json"}
        return await self._request_json(
            "POST", url, headers=headers, params=params, json=body
        )

    async def _upload_file_resumable(
        self, data: bytes, display_name: str, mime_type: str
    ) -> str:
        """
        Starts a resumable upload and then uploads+finalizes it.
        Returns the file_uri.
        """
        # 1) START
        start_url = f"{self.cfg.base_url}/upload/v1beta/files"
        start_headers = {
            **self._auth_headers(),
            "X-Goog-Upload-Protocol": "resumable",
            "X-Goog-Upload-Command": "start",
            "X-Goog-Upload-Header-Content-Length": str(len(data)),
            "X-Goog-Upload-Header-Content-Type": mime_type,
            "Content-Type": "application/json",
        }
        start_body = {"file": {"display_name": display_name}}
        r = await self._request(
            "POST", start_url, headers=start_headers, content=json.dumps(start_body)
        )
        upload_url = r.headers.get("x-goog-upload-url")
        if not upload_url:
            raise GeminiError(
                r.status_code, {"message": "Missing x-goog-upload-url in response"}
            )

        # 2) UPLOAD + FINALIZE
        upload_headers = {
            "X-Goog-Upload-Offset": "0",
            "X-Goog-Upload-Command": "upload, finalize",
            "Content-Type": mime_type,
        }
        r2 = await self._request(
            "POST", upload_url, headers=upload_headers, content=data
        )
        payload = r2.json()
        try:
            return payload["file"]["uri"]
        except Exception:
            raise GeminiError(r2.status_code, payload)

    async def _delete_file_by_uri(self, file_uri: str) -> None:
        """
        Deletes a file by its `file_uri` (format: files/abc123…). If deletion fails,
        we swallow errors by default (file auto-expires anyway).
        """
        # file_uri looks like "files/xxxxxxxx"
        file_id = file_uri.split("/", 1)[-1]
        url = f"{self.cfg.base_url}/v1beta/files/{file_id}"
        headers = self._auth_headers()
        r = await self._request("DELETE", url, headers=headers)
        if r.status_code not in (200, 204):
            # Not fatal for the caller
            pass

    # --------------- HTTP + retries ---------------

    async def _request(
        self,
        method: str,
        url: str,
        *,
        headers: Optional[Dict[str, str]] = None,
        params: Optional[Dict[str, str]] = None,
        json: Any = None,
        content: Any = None,
        max_retries: int = 3,
    ) -> httpx.Response:
        attempt = 0

        t0 = time.perf_counter()
        while True:
            attempt += 1
            resp = await self._client.request(
                method, url, headers=headers, params=params, json=json, content=content
            )
            if resp.status_code < 400:
                self._last_status_code = resp.status_code
                self._last_attempts = attempt
                self._last_latency_ms = (time.perf_counter() - t0) * 1000.0
                return resp

            if resp.status_code in (429, 500, 502, 503, 504) and attempt <= max_retries:
                retry_after = resp.headers.get("retry-after")
                delay = (
                    float(retry_after)
                    if retry_after and retry_after.isdigit()
                    else min(2.0 * attempt, 10.0)
                )
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
            raise GeminiError(resp.status_code, payload)

    async def _request_json(
        self,
        method: str,
        url: str,
        *,
        headers: Optional[Dict[str, str]] = None,
        params: Optional[Dict[str, str]] = None,
        json: Any = None,
        content: Any = None,
    ) -> Dict[str, Any]:
        r = await self._request(
            method, url, headers=headers, params=params, json=json, content=content
        )
        try:
            return r.json()
        except Exception:
            raise GeminiError(r.status_code, {"message": "Invalid JSON in response"})

    async def embed_text(
        self,
        text: str,
        *,
        model: str = "text-embedding-004",  # Google’s current text embedding model
        task_type: str | None = None,  # optional; e.g. "RETRIEVAL_DOCUMENT"
    ) -> list[float]:
        """
        Calls the Gemini embeddings API and returns a single vector (list of floats).
        """
        if not text:
            return []

        url = f"{self.cfg.base_url}/v1beta/models/{model}:embedContent"
        body = {"content": {"parts": [{"text": text}]}}
        if task_type:
            body["taskType"] = task_type

        # embeds use API key in query param too
        params = {"key": self.cfg.api_key}
        headers = {"Content-Type": "application/json"}
        resp = await self._request_json(
            "POST", url, headers=headers, params=params, json=body
        )

        # Response shape:
        # {"embedding": {"values": [float, float, ...]}}
        emb = ((resp or {}).get("embedding") or {}).get("values")
        if not isinstance(emb, list):
            raise GeminiError(500, {"message": "Invalid embedding response"})
        return emb
