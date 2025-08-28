# src/dal/remote/hackernews_adapter.py
from __future__ import annotations

from typing import Any, Dict, List, Optional, Tuple
from datetime import datetime, timezone, timedelta
import html
import re
import requests
from bs4 import BeautifulSoup

from src.dal.remote.base import BaseAdapter
from src.domain.models.preview_model import PreviewModel, EnumMode

HN_BASE = "https://hacker-news.firebaseio.com/v0"
ALGOLIA_BASE = "https://hn.algolia.com/api/v1"

FEEDS_MAP: Dict[str, str] = {
    "top":  "topstories",
    "new":  "newstories",
    "best": "beststories",
    "ask":  "askstories",
    "show": "showstories",
    "job":  "jobstories",
}

def _iso(ts: int | float | None) -> Optional[str]:
    if ts is None:
        return None
    return datetime.fromtimestamp(int(ts), tz=timezone.utc).isoformat()

def _strip_html_to_text(raw: Optional[str]) -> Optional[str]:
    if not raw:
        return None
    # HN text is HTML-ish; unescape then strip tags
    un = html.unescape(raw)
    soup = BeautifulSoup(un, "html.parser")
    txt = soup.get_text(" ", strip=True)
    # collapse whitespace
    return re.sub(r"\s+", " ", txt).strip()

class HackerNewsAdapter(BaseAdapter):
    """
    Unified 'topics' across HN feeds.
    - Numeric paging per feed by slicing the ID list.
    - Optional top-level comment excerpts for context.
    - Optional Algolia search returning the same TopicsModel shape.
    """
    item_name = "hacker_news"
    source_name = "apps"

    # --------- Preview ----------
    def get_preview(self) -> PreviewModel:
        return PreviewModel(
            mode=EnumMode.BOTH,
            source_name=self.source_name,
            has_topic=True,
            item_name=self.item_name,
            item_img="https://res.cloudinary.com/dhncdmb2t/image/upload/v1756375178/hn_fmfumb.png",
            updated_at=datetime.now(timezone.utc).isoformat(),
        )

    # --------- HTTP helpers ----------
    def _get_json(self, url: str, params: Optional[Dict[str, Any]] = None) -> Dict[str, Any] | List[Any]:
        r = requests.get(url, params=params or {}, timeout=15, headers={"User-Agent": "quiz-certify/1.0"})
        r.raise_for_status()
        return r.json()

    # --------- Firebase fetchers ----------
    def _fetch_ids(self, feed_key: str) -> List[int]:
        path = FEEDS_MAP.get(feed_key, FEEDS_MAP["top"])
        data = self._get_json(f"{HN_BASE}/{path}.json")
        return list(data) if isinstance(data, list) else []

    def _fetch_item(self, item_id: int) -> Dict[str, Any]:
        return dict(self._get_json(f"{HN_BASE}/item/{item_id}.json") or {})

    def _fetch_items(self, ids: List[int]) -> List[Dict[str, Any]]:
        out: List[Dict[str, Any]] = []
        for i in ids:
            try:
                it = self._fetch_item(i)
                if it:
                    out.append(it)
            except requests.HTTPError:
                continue
        return out

    def _fetch_user(self, user_id: str) -> Dict[str, Any]:
        return dict(self._get_json(f"{HN_BASE}/user/{user_id}.json") or {})

    # --------- Comments ----------
    def _fetch_top_comments(self, story: Dict[str, Any], limit: int) -> List[Dict[str, Any]]:
        kids = story.get("kids") or []
        if not kids or limit <= 0:
            return []
        top_ids = kids[:limit]
        comments = self._fetch_items(top_ids)
        out: List[Dict[str, Any]] = []
        for c in comments:
            if c.get("type") != "comment":
                continue
            out.append({
                "type": "comment",
                "id": c.get("id"),
                "by": c.get("by"),
                "time": c.get("time"),
                "time_iso": _iso(c.get("time")),
                "text": c.get("text"),
                "excerpt": _strip_html_to_text(c.get("text"))[:280] if c.get("text") else None,
                "parent": c.get("parent"),
                "replies_count": len(c.get("kids") or []),
            })
        return out

    # --------- Normalization ----------
    def _tag_from_title_or_type(self, item: Dict[str, Any]) -> str:
        t = (item.get("title") or "").strip().lower()
        typ = (item.get("type") or "").lower()
        if typ == "job":
            return "job"
        if t.startswith("ask hn"):
            return "ask"
        if t.startswith("show hn"):
            return "show"
        return "story"

    def _norm_story(self, item: Dict[str, Any], section: str, *, with_comments: bool, comments_n: int) -> Dict[str, Any]:
        tag = self._tag_from_title_or_type(item)
        text = item.get("text") if tag in ("ask", "show") else None
        body_excerpt = _strip_html_to_text(text)[:400] if text else None

        topic: Dict[str, Any] = {
            "type": "story",
            "section": section,                 # which feed we sliced from (top/new/best/ask/show/job)
            "tag": tag,                         # derived tag
            "id": item.get("id"),
            "title": item.get("title"),
            "url": item.get("url"),
            "text": text,                       # only for self-posts (ask/show)
            "text_excerpt": body_excerpt,       # quick context
            "score": item.get("score"),
            "time": item.get("time"),
            "time_iso": _iso(item.get("time")),
            "author": item.get("by"),
            "comment_count": item.get("descendants"),
        }

        if with_comments and comments_n > 0:
            topic["top_comments"] = self._fetch_top_comments(item, comments_n)

        return topic

    # --------- Public: Topics from feeds ----------
    def get_topics(
        self,
        *,
        page: int = 1,
        per_page: int = 30,
        feeds: Optional[List[str]] = None,   # e.g. ["top","new"] ; defaults below
        with_comments: bool = False,
        comments_per_story: int = 2,
        **_: Any
    ) -> Dict[str, Any]:
        """
        Numeric pagination over one or more HN feeds.
        If multiple feeds provided, we split per_page evenly (remainder to the first buckets)
        and merge results in feed order.
        """
        assert page >= 1 and per_page >= 1
        feeds = feeds or ["top", "new"]   # freshness+engagement by default

        base = per_page // len(feeds)
        remainder = per_page % len(feeds)
        quotas = [base + (1 if i < remainder else 0) for i in range(len(feeds))]

        merged: List[Dict[str, Any]] = []
        has_more_any = False

        for idx, feed in enumerate(feeds):
            quota = quotas[idx]
            if quota == 0:
                continue

            ids = self._fetch_ids(feed)
            start = (page - 1) * quota
            end = start + quota
            page_ids = ids[start:end]

            items = self._fetch_items(page_ids)
            for it in items:
                # normalize
                merged.append(self._norm_story(it, section=feed, with_comments=with_comments, comments_n=comments_per_story))

            # has_more if our slice didn't exhaust the feed
            if end < len(ids):
                has_more_any = True

        return {
            "topics": merged[:per_page],
            "page": page,
            "per_page": per_page,
            "has_more": has_more_any,
            "fetched_at": datetime.now(timezone.utc).isoformat(),
            "item_name": self.item_name,
            "source_name": self.source_name,
        }

    # --------- Optional: Algolia search (same unified TopicsModel) ----------
    def search_topics(
        self,
        *,
        query: str,
        page: int = 1,
        per_page: int = 30,
        author: Optional[str] = None,
        min_points: Optional[int] = None,
        since_days: Optional[int] = None,
        tags: Optional[List[str]] = None,  # e.g., ["story"], or ["ask_hn"], ["show_hn"]
    ) -> Dict[str, Any]:
        """
        Full-text HN search via Algolia (returns unified topics).
        Useful to match Google Trends or in-app queries.
        """
        assert page >= 1 and per_page >= 1
        params: Dict[str, Any] = {
            "query": query or "",
            "page": page - 1,            # Algolia pages are 0-based
            "hitsPerPage": per_page,
        }

        # tags
        # Examples: tags=story | ask_hn | show_hn | author_<name>
        algolia_tags: List[str] = []
        if tags:
            algolia_tags.extend(tags)
        if author:
            algolia_tags.append(f"author_{author}")
        if algolia_tags:
            params["tags"] = ",".join(algolia_tags)

        # numeric filters: points, created_at_i
        numeric_filters: List[str] = []
        if min_points is not None:
            numeric_filters.append(f"points>={int(min_points)}")
        if since_days is not None:
            since_ts = int((datetime.now(timezone.utc) - timedelta(days=int(since_days))).timestamp())
            numeric_filters.append(f"created_at_i>={since_ts}")
        if numeric_filters:
            params["numericFilters"] = ",".join(numeric_filters)

        data = self._get_json(f"{ALGOLIA_BASE}/search", params)  # search_by_date works too
        hits: List[Dict[str, Any]] = list(data.get("hits", []))

        topics: List[Dict[str, Any]] = []
        for h in hits:
            # Map Algolia fields to our normalized story shape
            tag = "story"
            title = h.get("title") or h.get("story_title")
            url = h.get("url") or h.get("story_url")
            text = h.get("story_text") or h.get("comment_text")
            created_i = h.get("created_at_i")
            author = h.get("author")
            points = h.get("points")
            num_comments = h.get("num_comments")

            # Derive ask/show from title (Algolia also has tags but we already normalized tags above)
            low_title = (title or "").lower()
            if low_title.startswith("ask hn"):
                tag = "ask"
            elif low_title.startswith("show hn"):
                tag = "show"

            topics.append({
                "type": "story",
                "section": "search",
                "tag": tag,
                "id": h.get("objectID"),
                "title": title,
                "url": url,
                "text": text,                              # self-post text if present
                "text_excerpt": (_strip_html_to_text(text)[:400] if text else None),
                "score": points,
                "time": created_i,
                "time_iso": _iso(created_i),
                "author": author,
                "comment_count": num_comments,
            })

        has_more = (page * per_page) < int(data.get("nbHits", 0))

        return {
            "topics": topics,
            "page": page,
            "per_page": per_page,
            "has_more": has_more,
            "fetched_at": datetime.now(timezone.utc).isoformat(),
            "item_name": self.item_name,
            "source_name": self.source_name,
        }
