# src/dal/remote/producthunt_adapter.py
from __future__ import annotations
from typing import Any, Dict, List, Optional, Tuple
from datetime import datetime, timezone
import time, requests

from src.dal.remote.base import BaseAdapter
from src.domain.models.preview_model import PreviewModel, EnumMode
from src.core.settings import app_settings

PH_GRAPHQL = "https://api.producthunt.com/v2/api/graphql"

class ProductHuntAdapter(BaseAdapter):
    item_name = "product_hunt"
    source_name = "apps"

    def get_preview(self) -> PreviewModel:
        return PreviewModel(
            mode=EnumMode.PLAYFUL,
            source_name=self.source_name,
            has_topic=True,
            item_name=self.item_name,
            item_img="https://res.cloudinary.com/dhncdmb2t/image/upload/v1756449289/product_hunt_bt96ah.png",
            updated_at=datetime.now(timezone.utc).isoformat(),
        )

    # ---- HTTP/GraphQL helpers ----
    def _headers(self) -> Dict[str, str]:
        s = app_settings()
        return {
            "Authorization": f"Bearer {s.PRODUCTHUNT_DEVELOPER_TOKEN}",
            "Content-Type": "application/json",
            "User-Agent": s.PRODUCTHUNT_USER_AGENT,
        }

    def _graphql(self, query: str, variables: Dict[str, Any]) -> Dict[str, Any]:
        for attempt in range(3):
            r = requests.post(PH_GRAPHQL, json={"query": query, "variables": variables},
                              headers=self._headers(), timeout=20)
            if r.status_code in (429, 500, 502, 503, 504):
                time.sleep(1 + attempt * 1.5)
                continue
            r.raise_for_status()
            data = r.json()
            if data.get("errors"):
                msg = data["errors"][0].get("message")
                raise RuntimeError(f"ProductHunt GraphQL error: {msg}")
            return data["data"]
        r.raise_for_status()  # just in case

    # ---- Topics paging (cursor → numeric) ----
    _Q_TOPICS = """
    query TopicsPage($first: Int!, $after: String) {
      topics(order: FOLLOWERS_COUNT, first: $first, after: $after) {
        edges {
          cursor
          node { id name slug description followersCount }
        }
        pageInfo { endCursor hasNextPage }
      }
    }
    """

    def _page_topics(self, *, first: int, after: Optional[str]) -> Tuple[List[Dict[str, Any]], Optional[str], bool]:
        d = self._graphql(self._Q_TOPICS, {"first": first, "after": after})
        t = d.get("topics") or {}
        edges = t.get("edges") or []
        pi = t.get("pageInfo") or {}
        items: List[Dict[str, Any]] = [{
            "name": e["node"].get("name"),
            "slug": e["node"].get("slug"),
            "description": e["node"].get("description"),
            "followers_count": e["node"].get("followersCount"),
        } for e in edges if e.get("node")]
        return items, pi.get("endCursor"), bool(pi.get("hasNextPage"))

    # Public: your Topics contract
    def get_topics(self, *, page: int = 1, per_page: int = 45, **_: Any) -> Dict[str, Any]:
        assert page >= 1 and per_page >= 1
        cursor: Optional[str] = None
        for _ in range(page - 1):
            _, cursor, has_next = self._page_topics(first=per_page, after=cursor)
            if not has_next:
                return {
                    "topics": [], "page": page, "per_page": per_page, "has_more": False,
                    "fetched_at": datetime.now(timezone.utc).isoformat(),
                    "item_name": self.item_name, "source_name": self.source_name,
                }
        topics, end_cursor, has_next = self._page_topics(first=per_page, after=cursor)
        return {
            "topics": topics,
            "page": page,
            "per_page": per_page,
            "has_more": has_next,
            "fetched_at": datetime.now(timezone.utc).isoformat(),
            "item_name": self.item_name,
            "source_name": self.source_name,
        }
