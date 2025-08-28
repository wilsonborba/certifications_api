# src/dal/remote/reddit_adapter.py
from __future__ import annotations

import time
from datetime import datetime, timezone
from typing import Any, Dict, List, Optional, Tuple

import requests

from src.core.settings import app_settings
from src.dal.remote.base import BaseAdapter
from src.domain.models.preview_model import EnumMode, PreviewModel


REDDIT_TOKEN_URL = "https://www.reddit.com/api/v1/access_token"
REDDIT_API_BASE = "https://oauth.reddit.com"


class RedditAdapter(BaseAdapter):
    """
    Trends kinds:
      - 'communities' -> popular subreddits (proxy for trending topics)
      - 'hot'         -> /r/all/hot (now-ish)
      - 'top'         -> /r/all/top?t=day|hour|week|month|year|all
    Pagination: pass 'after' from the previous call; 'limit' <= 100.
    """
    item_name = "reddit"
    source_name = "apps"

    # ---- keep your preview exactly as-is ----
    def get_preview(self) -> PreviewModel:
        return PreviewModel(
            mode=EnumMode.PLAYFUL,
            source_name=self.source_name,
            has_topic=True,
            item_name=self.item_name,
            item_img="https://res.cloudinary.com/dhncdmb2t/image/upload/v1756293205/reddit_logo_t93flf.png",
            updated_at=datetime.now(timezone.utc).isoformat()
        )

    # ===== Internal OAuth & HTTP =====
    _token: Optional[str] = None
    _token_expiry: float = 0.0

    @property
    def _ua(self) -> str:
        return app_settings().REDDIT_USER_AGENT

    def _get_token(self) -> str:
        """Lazy OAuth. Supports client_credentials or installed_client (device_id)."""
        if self._token and time.time() < self._token_expiry - 30:
            return self._token

        s = app_settings()
        headers = {"User-Agent": self._ua}
        if s.REDDIT_CLIENT_SECRET:
            # web/script app
            auth = (s.REDDIT_CLIENT_ID or "", s.REDDIT_CLIENT_SECRET)
            data = {"grant_type": "client_credentials", "scope": s.REDDIT_SCOPE}
        else:
            # installed app
            auth = (s.REDDIT_CLIENT_ID or "", "")
            data = {
                "grant_type": "https://oauth.reddit.com/grants/installed_client",
                "device_id": s.REDDIT_DEVICE_ID or "DO_NOT_TRACK_THIS_DEVICE",
                "scope": s.REDDIT_SCOPE,
            }

        resp = requests.post(REDDIT_TOKEN_URL, data=data, headers=headers, auth=auth, timeout=15)
        resp.raise_for_status()
        payload = resp.json()
        self._token = payload["access_token"]
        self._token_expiry = time.time() + int(payload.get("expires_in", 3600))
        return self._token

    def _get(self, path: str, params: Dict[str, Any] | None = None) -> Dict[str, Any]:
        token = self._get_token()
        headers = {"Authorization": f"Bearer {token}", "User-Agent": self._ua}
        url = f"{REDDIT_API_BASE}{path}"
        resp = requests.get(url, headers=headers, params=params or {}, timeout=20)
        resp.raise_for_status()
        return resp.json()

    def _page(
        self,
        path: str,
        limit: int = 25,
        after: str | None = None,
        **extra_params: Any,
    ) -> Tuple[List[Dict[str, Any]], Optional[str]]:
        """Fetch ONE Listing page; returns (children_items, next_after)."""
        limit = max(1, min(limit, 100))
        params: Dict[str, Any] = {"limit": limit, **extra_params}
        if after:
            params["after"] = after
        data = self._get(path, params=params)
        d = data.get("data", {}) or {}
        return d.get("children", []), d.get("after")

    # ===== Private kind handlers =====
    def _kind_communities(self, *, limit: int, after: str | None) -> Dict[str, Any]:
        # correct endpoint + lowercase sr_detail
        children, next_after = self._page(
            "/subreddits/popular",
            limit=limit,
            after=after,
            sr_detail="true",
        )
        items = []
        for c in children:
            d = c.get("data", {}) or {}
            items.append({
                "type": "subreddit",
                "name": d.get("display_name_prefixed") or d.get("display_name"),
                "title": d.get("title"),
                "subscribers": d.get("subscribers"),
                "url": f"https://www.reddit.com{d.get('url')}" if d.get("url") else None,
                "icon_img": d.get("community_icon") or d.get("icon_img"),
                "nsfw": d.get("over18"),
            })
        return {
            "items": items,
            "after": next_after,
            "kind": "communities",
            "fetched_at": datetime.now(timezone.utc).isoformat(),
        }

    def _kind_hot(self, *, limit: int, after: str | None) -> Dict[str, Any]:
        children, next_after = self._page("/r/all/hot", limit=limit, after=after)
        items = []
        for c in children:
            d = c.get("data", {}) or {}
            items.append({
                "type": "post",
                "title": d.get("title"),
                "subreddit": d.get("subreddit_name_prefixed"),
                "score": d.get("score"),
                "num_comments": d.get("num_comments"),
                "url": f"https://www.reddit.com{d.get('permalink')}" if d.get("permalink") else d.get("url"),
                "created_utc": d.get("created_utc"),
                "author": d.get("author"),
                "thumbnail": d.get("thumbnail") if (d.get("thumbnail") or "").startswith("http") else None,
                "nsfw": d.get("over_18"),
            })
        return {
            "items": items,
            "after": next_after,
            "kind": "hot",
            "fetched_at": datetime.now(timezone.utc).isoformat(),
        }

    def _kind_top(self, *, limit: int, after: str | None, time_window: str | None) -> Dict[str, Any]:
        t = (time_window or "day").lower()
        children, next_after = self._page("/r/all/top", limit=limit, after=after, t=t)
        items = []
        for c in children:
            d = c.get("data", {}) or {}
            items.append({
                "type": "post",
                "title": d.get("title"),
                "subreddit": d.get("subreddit_name_prefixed"),
                "score": d.get("score"),
                "num_comments": d.get("num_comments"),
                "url": f"https://www.reddit.com{d.get('permalink')}" if d.get("permalink") else d.get("url"),
                "created_utc": d.get("created_utc"),
                "author": d.get("author"),
                "thumbnail": d.get("thumbnail") if (d.get("thumbnail") or "").startswith("http") else None,
                "nsfw": d.get("over_18"),
                "time_window": t,
            })
        return {
            "items": items,
            "after": next_after,
            "kind": "top",
            "fetched_at": datetime.now(timezone.utc).isoformat(),
        }

    # ===== Public: GetTrends (thin dispatcher) =====
    def get_trends(
        self,
        kind: str = "communities",
        *,
        limit: int = 25,
        after: str | None = None,
        time_window: str | None = None,
    ) -> Dict[str, Any]:
        kind = (kind or "communities").lower()
        if kind == "communities":
            return self._kind_communities(limit=limit, after=after)
        if kind == "hot":
            return self._kind_hot(limit=limit, after=after)
        if kind == "top":
            return self._kind_top(limit=limit, after=after, time_window=time_window)
        raise ValueError("kind must be one of: 'communities', 'hot', 'top'")
