from __future__ import annotations

import hashlib
import hmac
from datetime import UTC, datetime
from urllib.parse import quote, urlsplit

import httpx


class FsmConfigurationError(RuntimeError):
    pass


class FsmStorageError(RuntimeError):
    pass


class FsmS3Adapter:
    """Minimal header-signed FSM S3 client for PUT/DELETE logical objects."""

    def __init__(self, *, endpoint: str, access_key: str | None, secret_key: str | None, region: str, bucket: str) -> None:
        self._endpoint = endpoint.rstrip("/")
        self._access_key = access_key
        self._secret_key = secret_key
        self._region = region
        self._bucket = bucket

    def _authorization(self, *, method: str, path: str, payload_hash: str, content_type: str) -> dict[str, str]:
        if not self._access_key or not self._secret_key:
            raise FsmConfigurationError("FSM storage is not configured")
        now = datetime.now(UTC)
        stamp = now.strftime("%Y%m%dT%H%M%SZ")
        date = now.strftime("%Y%m%d")
        host = urlsplit(self._endpoint).netloc
        headers = {"content-type": content_type, "host": host, "x-amz-content-sha256": payload_hash, "x-amz-date": stamp}
        canonical_headers = "".join(f"{key}:{headers[key]}\n" for key in sorted(headers))
        signed_headers = ";".join(sorted(headers))
        canonical = f"{method}\n{path}\n\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
        scope = f"{date}/{self._region}/s3/aws4_request"
        string_to_sign = f"AWS4-HMAC-SHA256\n{stamp}\n{scope}\n{hashlib.sha256(canonical.encode()).hexdigest()}"
        key = ("AWS4" + self._secret_key).encode()
        for value in (date, self._region, "s3", "aws4_request"):
            key = hmac.new(key, value.encode(), hashlib.sha256).digest()
        signature = hmac.new(key, string_to_sign.encode(), hashlib.sha256).hexdigest()
        headers["authorization"] = f"AWS4-HMAC-SHA256 Credential={self._access_key}/{scope}, SignedHeaders={signed_headers}, Signature={signature}"
        return headers

    def object_key(self, *, study_id: str, source_id: str, filename: str) -> str:
        safe_name = filename.rsplit("/", 1)[-1].replace("\\", "_")
        return f"studies/{study_id}/sources/{source_id}/{safe_name}"

    async def put(self, *, key: str, body: bytes, content_type: str) -> None:
        encoded_key = quote(key, safe="/")
        path = f"/{self._bucket}/{encoded_key}"
        payload_hash = hashlib.sha256(body).hexdigest()
        headers = self._authorization(method="PUT", path=path, payload_hash=payload_hash, content_type=content_type)
        try:
            async with httpx.AsyncClient(timeout=90.0) as client:
                response = await client.put(f"{self._endpoint}{path}", content=body, headers=headers)
        except httpx.HTTPError as exc:
            raise FsmStorageError("FSM upload failed") from exc
        if response.status_code not in {200, 201, 204}:
            raise FsmStorageError("FSM rejected the upload")

    async def delete(self, *, key: str) -> None:
        path = f"/{self._bucket}/{quote(key, safe='/')}"
        headers = self._authorization(method="DELETE", path=path, payload_hash=hashlib.sha256(b"").hexdigest(), content_type="application/octet-stream")
        try:
            async with httpx.AsyncClient(timeout=30.0) as client:
                response = await client.delete(f"{self._endpoint}{path}", headers=headers)
        except httpx.HTTPError as exc:
            raise FsmStorageError("FSM delete failed") from exc
        if response.status_code not in {200, 204, 404}:
            raise FsmStorageError("FSM rejected deletion")

    async def get(self, *, key: str) -> bytes:
        path = f"/{self._bucket}/{quote(key, safe='/')}"
        headers = self._authorization(method="GET", path=path, payload_hash=hashlib.sha256(b"").hexdigest(), content_type="application/octet-stream")
        try:
            async with httpx.AsyncClient(timeout=90.0) as client:
                response = await client.get(f"{self._endpoint}{path}", headers=headers)
        except httpx.HTTPError as exc:
            raise FsmStorageError("FSM download failed") from exc
        if response.status_code != 200:
            raise FsmStorageError("FSM object is unavailable")
        return response.content
