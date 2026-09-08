from urllib.parse import quote

import httpx


class FsmConfigurationError(RuntimeError):
    pass


class FsmStorageError(RuntimeError):
    pass


class FsmMediaAdapter:
    """Server-only client for FSM's bearer-token Media API.

    FSM owns the media key; Certifications persists that opaque key and never
    exposes the app credential to Flutter.
    """

    def __init__(
        self,
        *,
        endpoint: str,
        app: str,
        app_key: str | None,
        transport: httpx.AsyncBaseTransport | None = None,
    ) -> None:
        self._endpoint = endpoint.rstrip("/")
        self._app = app
        self._app_key = app_key
        self._transport = transport

    def _headers(self) -> dict[str, str]:
        if not self._app_key:
            raise FsmConfigurationError("FSM media storage is not configured")
        return {"Authorization": f"Bearer {self._app_key}"}

    @staticmethod
    def album(study_id: str) -> str:
        return f"study-{study_id}"

    async def upload(self, *, album: str, filename: str, body: bytes, content_type: str) -> str:
        try:
            async with httpx.AsyncClient(timeout=90.0, transport=self._transport) as client:
                response = await client.post(
                    f"{self._endpoint}/{self._app}/media",
                    headers=self._headers(),
                    data={"album": album, "force": "false"},
                    files={"file": (filename, body, content_type)},
                )
        except httpx.HTTPError as exc:
            raise FsmStorageError("FSM upload failed") from exc
        if response.status_code not in {200, 201}:
            raise FsmStorageError("FSM rejected the upload")
        try:
            key = str(response.json()["item"]["key"])
        except (KeyError, TypeError, ValueError) as exc:
            raise FsmStorageError("FSM returned an invalid upload response") from exc
        if not key:
            raise FsmStorageError("FSM returned an empty media key")
        return key

    async def get(self, *, key: str) -> bytes:
        try:
            async with httpx.AsyncClient(timeout=90.0, transport=self._transport) as client:
                response = await client.get(
                    f"{self._endpoint}/{self._app}/media/{quote(key, safe='')}", headers=self._headers()
                )
        except httpx.HTTPError as exc:
            raise FsmStorageError("FSM download failed") from exc
        if response.status_code != 200:
            raise FsmStorageError("FSM object is unavailable")
        return response.content

    async def delete(self, *, key: str) -> None:
        try:
            async with httpx.AsyncClient(timeout=30.0, transport=self._transport) as client:
                response = await client.delete(
                    f"{self._endpoint}/{self._app}/media/{quote(key, safe='')}", headers=self._headers()
                )
        except httpx.HTTPError as exc:
            raise FsmStorageError("FSM delete failed") from exc
        if response.status_code not in {200, 204, 404}:
            raise FsmStorageError("FSM rejected deletion")
