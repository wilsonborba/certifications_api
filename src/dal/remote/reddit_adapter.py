# src/dal/remote/reddit_adapter.py
from __future__ import annotations

import time
from datetime import datetime, timezone
from typing import Any, Dict, Optional, Tuple, List
import requests

from src.core.settings import app_settings
from src.dal.remote.base import BaseAdapter
from src.domain.models.preview_model import EnumMode, PreviewModel

REDDIT_TOKEN_URL = "https://www.reddit.com/api/v1/access_token"
REDDIT_API_BASE = "https://oauth.reddit.com"

class RedditAdapter(BaseAdapter):
    item_name = "reddit"
    source_name = "apps"

    def get_preview(self) -> PreviewModel:
        return PreviewModel(
            mode=EnumMode.PLAYFUL,
            source_name=self.source_name,
            has_topic=True,
            item_name=self.item_name,
            item_img="https://res.cloudinary.com/dhncdmb2t/image/upload/v1756293205/reddit_logo_t93flf.png",
            updated_at=datetime.now(timezone.utc).isoformat()
        )

    _token: Optional[str] = None
    _token_expiry: float = 0.0

    @property
    def _ua(self) -> str:
        return app_settings().REDDIT_USER_AGENT

    def _get_token(self) -> str:
        if self._token and time.time() < self._token_expiry - 30:
            return self._token
        s = app_settings()
        headers = {"User-Agent": self._ua}
        if s.REDDIT_CLIENT_SECRET:
            auth = (s.REDDIT_CLIENT_ID or "", s.REDDIT_CLIENT_SECRET)
            data = {"grant_type": "client_credentials", "scope": s.REDDIT_SCOPE}
        else:
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

    def _page(self, path: str, *, limit: int, after: str | None = None, **extra: Any) -> tuple[list[dict], str | None]:
        limit = max(1, min(limit, 100))
        params = {"limit": limit, **extra}
        if after:
            params["after"] = after
        data = self._get(path, params=params).get("data", {}) or {}
        return data.get("children", []), data.get("after")

    # ----- per-kind helpers -----
    def _kind_communities(self, *, limit: int, after: str | None) -> tuple[list[dict], str | None]:
        children, next_after = self._page("/subreddits/popular", limit=limit, after=after, sr_detail="true")
        trends: list[dict] = []
        for c in children:
            d = c.get("data", {}) or {}
            trends.append({
                "type": "subreddit",
                "name": d.get("display_name_prefixed") or d.get("display_name"),
                "title": d.get("title"),
                "subscribers": d.get("subscribers"),
                "url": f"https://www.reddit.com{d.get('url')}" if d.get("url") else None,
                "icon_img": d.get("community_icon") or d.get("icon_img"),
                "nsfw": d.get("over18"),
            })
        return trends, next_after

    def _kind_hot(self, *, limit: int, after: str | None) -> tuple[list[dict], str | None]:
        children, next_after = self._page("/r/all/hot", limit=limit, after=after)
        trends: list[dict] = []
        for c in children:
            d = c.get("data", {}) or {}
            trends.append({
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
        return trends, next_after

    def _kind_top(self, *, limit: int, after: str | None, time_window: str | None) -> tuple[list[dict], str | None]:
        t = (time_window or "day").lower()
        children, next_after = self._page("/r/all/top", limit=limit, after=after, t=t)
        trends: list[dict] = []
        for c in children:
            d = c.get("data", {}) or {}
            trends.append({
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
        return trends, next_after

    # ----- public: unified, numeric pagination -----
    def get_topics(
        self,
        *,
        page: int = 1,
        per_page: int = 30,
        time_window: str | None = None,   # adapter-specific; optional
        **_: Any
    ) -> Dict[str, Any]:
        """
        Build a mixed page (top + hot + communities) using numeric paging.
        We walk (page-1) cursor pages internally per kind, then fetch the page.
        """
        assert page >= 1 and per_page >= 1

        kinds = ["top", "hot", "communities"]
        base = per_page // len(kinds)
        remainder = per_page % len(kinds)
        per_kind_limits = [base + (1 if i < remainder else 0) for i in range(len(kinds))]

        # helper: returns (trends, has_more) for a given kind at numeric page
        def page_for_kind(kind: str, k_per_page: int) -> tuple[list[dict], bool]:
            if kind == "top":
                handler = lambda after: self._kind_top(limit=k_per_page, after=after, time_window=time_window)
            elif kind == "hot":
                handler = lambda after: self._kind_hot(limit=k_per_page, after=after)
            else:
                handler = lambda after: self._kind_communities(limit=k_per_page, after=after)

            cursor = None
            for _ in range(page - 1):
                _, cursor = handler(cursor)
                if not cursor:
                    break
            trends, next_cursor = handler(cursor)
            return trends, bool(next_cursor)

        merged: list[dict] = []
        any_has_more = False
        for idx, k in enumerate(kinds):
            t, has_more = page_for_kind(k, per_kind_limits[idx])
            merged.extend(t)
            any_has_more = any_has_more or has_more

        return {
            "topics": merged,
            "page": page,
            "per_page": per_page,
            "has_more": any_has_more,
            "updated_at": datetime.now(timezone.utc).isoformat(),
            "item_name": self.item_name,
            "source_name": self.source_name,
        }
