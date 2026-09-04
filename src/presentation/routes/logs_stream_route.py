from __future__ import annotations

from fastapi import APIRouter, WebSocket, WebSocketDisconnect

from src.core.logs import _tail
from src.core.settings import app_settings

logs_router = APIRouter(tags=["logs"])


@logs_router.websocket("/logs/stream")
async def stream_logs(websocket: WebSocket) -> None:
    """Streams the certifications.log file live, line by line.
    Clients connect and receive new log lines via WebSocket text messages.
    """
    await websocket.accept()
    settings = app_settings()
    try:
        async for line in _tail(settings.LOG_FILE):
            await websocket.send_text(line)
    except WebSocketDisconnect:
        pass
