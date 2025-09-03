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
                norm = self._norm_story(it, section=feed, with_comments=with_comments, comments_n=comments_per_story)
                norm["input_identification"] = str(norm.get("id")) if norm.get("id") is not None else None
                norm["topic_type"] = "story"
                merged.append(norm)

            # has_more if our slice didn't exhaust the feed
            if end < len(ids):
                has_more_any = True

        return {
            "topics": merged[:per_page],
            "page": page,
            "per_page": per_page,
            "has_more": has_more_any,
            "updated_at": datetime.now(timezone.utc).isoformat(),
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

            topic = {
                "type": "story",
                "section": "search",
                "tag": tag,
                "id": h.get("objectID"),
                "title": title,
                "url": url,
                "text": text,
                "text_excerpt": (_strip_html_to_text(text)[:400] if text else None),
                "score": points,
                "time": created_i,
                "time_iso": _iso(created_i),
                "author": author,
                "comment_count": num_comments,
            }
            topic["input_identification"] = str(topic["id"]) if topic.get("id") is not None else None
            topic["topic_type"] = "story"
            topics.append(topic)

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

    # ---------- Input: full item fetch ----------
    def get_input(
        self,
        *,
        input_identification: str | int | None = None,
        comments_limit: int = 20,
        include_comments: bool = True,
        include_external_content: bool = True,   # NEW: toggle fetching external article
        include_hn_page_text: bool = False,      # optional: fetch HN discussion page HTML as plain text
        **_: Any,
    ) -> Dict[str, Any]:
        now_iso = datetime.now(timezone.utc).isoformat()
        if input_identification is None:
            return {
                "input_identification": "",
                "input_data": {"error": "missing_input_identification"},
                "updated_at": now_iso,
            }

        try:
            sid = int(str(input_identification).strip())
        except ValueError:
            return {
                "input_identification": str(input_identification),
                "input_data": {"error": "invalid_story_id"},
                "updated_at": now_iso,
            }

        try:
            item = self._fetch_item(sid)
        except requests.HTTPError:
            item = {}

        if not item:
            return {
                "input_identification": str(sid),
                "input_data": {"error": "not_found"},
                "updated_at": now_iso,
            }

        tag = self._tag_from_title_or_type(item)
        text = item.get("text") if tag in ("ask", "show") else None
        text_clean = _strip_html_to_text(text) if text else None

        # optional comments
        top_comments: List[Dict[str, Any]] = []
        if include_comments and comments_limit > 0 and item.get("kids"):
            for c in self._fetch_top_comments(item, comments_limit):
                top_comments.append({
                    "id": c.get("id"),
                    "author": c.get("by"),
                    "time_iso": c.get("time_iso"),
                    "text_html": c.get("text"),
                    "text_excerpt": c.get("excerpt"),
                    "replies_count": c.get("replies_count"),
                })

        # NEW: fetch external article plaintext
        external_url = item.get("url")
        external_plaintext = None
        if include_external_content and external_url:
            external_plaintext = self._fetch_plaintext_from_url(external_url)

        # OPTIONAL: fetch HN discussion page plaintext (as a fallback / extra context)
        hn_page_plaintext = None
        if include_hn_page_text:
            hn_discussion_url = f"https://news.ycombinator.com/item?id={item.get('id')}"
            hn_page_plaintext = self._fetch_plaintext_from_url(hn_discussion_url, max_chars=80_000)

        input_data = {
            "meta": {
                "source": "Hacker News",
                "section": item.get("section"),     # may be None
                "tag": tag,                         # story/ask/show/job
                "story_id": item.get("id"),
                "story_url": f"https://news.ycombinator.com/item?id={item.get('id')}",
                "external_url": external_url,
                "author": item.get("by"),
                "points": item.get("score"),
                "comment_count": item.get("descendants"),
                "time_iso": _iso(item.get("time")),
                "title": item.get("title"),
            },
            "story": {
                "title": item.get("title"),
                "external_url": external_url,
                "self_text_html": text,        # original HTML-ish (Ask/Show)
                "self_text": text_clean,       # plain text body if self-post
            },
            "top_comments": top_comments,
        }

        # Attach new plaintext sections when available
        if external_plaintext:
            input_data["external_content"] = {
                "url": external_url,
                "text": external_plaintext,
            }
        if hn_page_plaintext:
            input_data["hn_page_content"] = {
                "url": f"https://news.ycombinator.com/item?id={item.get('id')}",
                "text": hn_page_plaintext,
            }

        return {
            "input_identification": str(sid),
            "input_data": input_data,
            "updated_at": now_iso,
        }

    def instructions(self) -> str:
        return (
            "You will receive a Hacker News story with optional self-text and a few top comments. "
            "Write engaging quiz questions that work in playful or serious modes.\n"
            "\n"
            "Guidelines:\n"
            "• Be precise with facts (titles, links, authors, points, dates, quotes).\n"
            "• Creativity is welcome in wording, but never change or invent facts.\n"
            "• You may add widely-known background context only if it clearly aligns with the provided content.\n"
            "• Great question ideas: identify the main claim, who/what/when, compare viewpoints in comments, "
            "interpret brief quotes, or order facts by magnitude/time.\n"
            "• Avoid speculation about future outcomes or unverifiable claims.\n"
            "• Keep each question answerable using the given context."
        )
    
    def generate_context(self, input_data: Dict[str, Any], amount_question: int = 10) -> str:
        meta = (input_data or {}).get("meta", {})
        story = (input_data or {}).get("story", {})
        comments = (input_data or {}).get("top_comments", []) or []
        ext = (input_data or {}).get("external_content", {})
        hnpg = (input_data or {}).get("hn_page_content", {})

        lines: List[str] = []
        lines.append("Hacker News Context")
        if meta.get("title"):        lines.append(f"Title: {meta['title']}")
        if meta.get("tag"):          lines.append(f"Type: {meta['tag']}")
        if meta.get("author"):       lines.append(f"Author: {meta['author']}")
        if meta.get("points") is not None: lines.append(f"Points: {meta['points']}")
        if meta.get("comment_count") is not None: lines.append(f"Comments: {meta['comment_count']}")
        if meta.get("time_iso"):     lines.append(f"Posted: {meta['time_iso']}")
        if meta.get("story_url"):    lines.append(f"HN Link: {meta['story_url']}")
        if meta.get("external_url"): lines.append(f"External URL: {meta['external_url']}")
        lines.append("")

        # Self-post body
        if story.get("self_text"):
            lines.append("Self-post body (plain text):")
            lines.append(story["self_text"])
            lines.append("")

        # External article content
        if ext.get("text"):
            lines.append("External article (plain text):")
            lines.append(ext["text"])
            lines.append("")

        # HN page text (optional, if you enabled it)
        if hnpg.get("text"):
            lines.append("HN discussion page (plain text):")
            lines.append(hnpg["text"])
            lines.append("")

        # Comment excerpts
        if comments:
            lines.append("Top comments (excerpts):")
            for c in comments[:10]:
                who = c.get("author") or "anon"
                ex = c.get("text_excerpt") or ""
                when = c.get("time_iso") or ""
                lines.append(f"- {who} ({when}): {ex}")
            lines.append("")

        lines.append("Focus your questions on facts present here. "
                    "Playful phrasing is okay; avoid speculation.")

        context = "\n".join(lines)
        context += self.context_output_structure(amount_question=amount_question)
        return context

    def _fetch_plaintext_from_url(
        self,
        url: str,
        *,
        timeout: int = 15,
        max_chars: int = 120_000,
    ) -> Optional[str]:
        """
        Fetch a URL and return readable plaintext:
        - handles text/html and text/plain
        - strips <script>/<style> and common chrome (nav/footer/aside)
        - collapses whitespace
        - caps output to max_chars to keep prompts reasonable
        """
        if not url:
            return None
        try:
            r = requests.get(url, timeout=timeout, headers={"User-Agent": "quiz-certify/1.0"})
            r.raise_for_status()
            ctype = (r.headers.get("Content-Type") or "").lower()
            # If it's already text/plain
            if "text/plain" in ctype and isinstance(r.text, str):
                txt = r.text
            else:
                # Treat everything else as HTML (common case)
                soup = BeautifulSoup(r.text or "", "html.parser")

                # Nuke obvious non-content
                for sel in ["script", "style", "noscript", "template", "svg"]:
                    for t in soup.select(sel):
                        t.decompose()
                for sel in ["nav", "footer", "header", "aside", ".sidebar", ".site-footer", ".site-header"]:
                    for t in soup.select(sel):
                        t.decompose()

                # Prefer main/article if present, else whole body
                scope = soup.select_one("main") or soup.select_one("article") or soup.body or soup
                txt = scope.get_text(" ", strip=True)

            # Unescape HTML entities and normalize whitespace
            txt = html.unescape(txt)
            txt = re.sub(r"\s+", " ", txt).strip()
            if not txt:
                return None
            if len(txt) > max_chars:
                txt = txt[:max_chars].rstrip() + " …"
            return txt
        except Exception:
            return None