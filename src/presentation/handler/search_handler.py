from __future__ import annotations

from typing import Any, Dict, Literal, Tuple
from src.domain.services.quiz_api_manager import QuizAPIManager
from src.core.logs import info, debug, error

# Modes your adapters support today
SearchMode = Literal["fulltext", "substring", "fuzzy"]

_manager = QuizAPIManager()

def search_from_app(
    *,
    item_name: str,
    q: str,
    page: int,
    per_page: int,
    mode: SearchMode,
    time_window: str | None,
    fill_page: bool = True,
    max_extra_pages: int = 2,
) -> Dict[str, Any]:
    """
    Orchestrates a single-adapter search and returns the unified envelope expected by the front.
    - Keeps the same structure as get_topics (topics[], page, per_page, has_more...).
    - Uses the adapter.search(...) that you just implemented (e.g., RedditAdapter.search).
    """

    # --- basic validation (keep it fast & explicit) ---
    q_norm = (q or "").strip()
    if not q_norm:
        raise ValueError("Query 'q' must be a non-empty string.")

    if page < 1:
        raise ValueError("Page must be >= 1.")
    if per_page < 1 or per_page > 300:
        # mirrors your topics route constraint style
        raise ValueError("per_page must be between 1 and 300.")

    if mode not in ("fulltext", "substring", "fuzzy"):
        raise ValueError("mode must be one of: fulltext | substring | fuzzy.")

    # --- (future) cache hook: look up an aggregate cache before hitting adapter ---
    # cache_key = f"fhapi:search:v1:{item_name}|{mode}|{page}|{per_page}|{hash(q_norm)}"
    # cached = await redis.get(cache_key)  # if/when you wire RedisAdapter in presentation layer
    # if cached: return cached

    # --- call adapter via manager ---
    # We keep the 'search' logic inside adapters. The manager resolves the adapter and delegates.
    try:
        info(f"[search] item={item_name} page={page} per_page={per_page} mode={mode} q='{q_norm}'")
        # Implemented below in manager (get_search), thin wrapper over adapter.search(...)
        payload = _manager.search(
            item_name=item_name,
            q=q_norm,
            page=page,
            per_page=per_page,
            mode=mode,
            time_window=time_window,
            fill_page=fill_page,
            max_extra_pages=max_extra_pages,
        )
        debug(f"[search] item={item_name} -> {len(payload.get('topics', []))} topics")
    except Exception as ex:
        error(f"[search] failure: {ex}")
        # Bubble up to route to map to HTTP (422/500/504 as you like)
        raise

    # --- (future) cache hook: persist briefly ---
    # await redis.set(cache_key, payload, ex=300)  # 5m TTL, adjust later

    return payload
