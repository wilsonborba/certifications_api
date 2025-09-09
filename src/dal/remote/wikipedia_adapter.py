# src/dal/remote/wikipedia_adapter.py
from __future__ import annotations

from time import time
from typing import Any, Dict, List, Optional, Tuple
from datetime import datetime, timezone, timedelta
from urllib.parse import urlparse, unquote
import re
import requests

from src.dal.remote.base import BaseAdapter
from src.domain.models.preview_model import PreviewModel, EnumMode

# --- API endpoints ---
MW_API = "https://en.wikipedia.org/w/api.php"  # MediaWiki action API
REST_SUMMARY = "https://en.wikipedia.org/api/rest_v1/page/summary/{title}"
REST_RELATED = "https://en.wikipedia.org/api/rest_v1/page/related/{title}"
PAGEVIEWS_TOP = "https://wikimedia.org/api/rest_v1/metrics/pageviews/top/{project}/{access}/{year}/{month}/{day}"
# Fallback: Featured feed with "mostread"
FEED_FEATURED = "https://en.wikipedia.org/api/rest_v1/feed/featured/{year}/{month}/{day}"

def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()

# Very small HTML tag stripper for safe context
_TAG_RE = re.compile(r"<[^>]+>")

def _strip_html(s: Optional[str]) -> str:
    if not s:
        return ""
    return _TAG_RE.sub("", s).strip()

def _yesterday_utc() -> datetime:
    # Pageviews "today" can be incomplete; use yesterday UTC by default
    return datetime.now(timezone.utc).date() - timedelta(days=1)

class WikipediaAdapter(BaseAdapter):
    """
    A playful/factual adapter around Wikipedia with:
      - get_topics: Top viewed pages (yesterday) and/or search results
      - get_input: Canonical page payload (summary, extract, sections, categories, related, thumb)
      - generate_context: Clean, compact prompt context for quiz generation
    """
    item_name = "wikipedia"
    source_name = "public_and_gov"

    # ---------------- preview ----------------
    def get_preview(self) -> PreviewModel:
        return PreviewModel(
            mode=EnumMode.BOTH,
            source_name=self.source_name,
            has_topic=True,
            item_name=self.item_name,
            item_img="https://upload.wikimedia.org/wikipedia/commons/6/63/Wikipedia-logo.png",
            updated_at=_now_iso(),
        )

    # ---------------- instructions ----------------
    def instructions(self) -> str:
        return (
            "You are given a Wikipedia article with summary and key sections. "
            "Create clear, factual, and non-controversial quiz questions based ONLY on the provided content. "
            "Focus on who/what/when/where facts, definitions, notable events, and widely accepted details. "
            "Avoid subjective or disputed claims. Keep questions concise and answerable from the context."
        )

    # ---------------- low-level helpers ----------------
    @property
    def _ua(self) -> str:
        # Configure in your settings; Wikimedia requires a descriptive UA with contact
        # e.g., WIKIMEDIA_USER_AGENT="Asodya/1.0 (https://yourdomain; contact@yourdomain)"
        return "AsodyaBot/1.0 (https://asodya.com; support@asodya.com)"
    


    # ---------------- low-level helpers ----------------
    def _mw_get(self, params: Dict[str, Any]) -> Dict[str, Any]:
        p = {"format": "json", "formatversion": 2, **params}
        r = requests.get(
            MW_API,
            params=p,
            headers={"User-Agent": self._ua, "Accept": "application/json"},
            timeout=20,
        )
        r.raise_for_status()
        return r.json()

    def _rest_get(self, url: str) -> Dict[str, Any]:
        r = requests.get(
            url,
            headers={"User-Agent": self._ua, "Accept": "application/json"},
            timeout=20,
        )
        r.raise_for_status()
        return r.json()

    # Parse a Wikipedia URL to Title (enwiki only)
    @staticmethod
    def _title_from_url(url: str) -> Optional[str]:
        try:
            u = urlparse(url)
            if "wikipedia.org" not in (u.netloc or ""):
                return None
            # /wiki/Albert_Einstein  or /w/index.php?title=...
            if u.path.startswith("/wiki/"):
                return unquote(u.path.split("/wiki/")[1])
            # fallback query param
            qs = dict(q.split("=", 1) for q in (u.query.split("&") if u.query else []) if "=" in q)
            t = qs.get("title")
            return unquote(t) if t else None
        except Exception:
            return None
        
    def _fetch_related_pages(self, title: str, *, limit: int = 10, thumb_size: int = 240) -> List[Dict[str, Any]]:
        """
        Use MediaWiki search 'morelike:<title>' to approximate related pages.
        Returns [{title, description, url, thumbnail}] up to 'limit'.
        """
        try:
            res = self._mw_get({
                "action": "query",
                "generator": "search",
                "gsrsearch": f"morelike:{title}",
                "gsrlimit": min(limit, 50),
                "gsrnamespace": 0,
                "prop": "description|pageimages|info",
                "inprop": "url",
                "pithumbsize": thumb_size,
            })
            pages = (res.get("query") or {}).get("pages") or []
            related: List[Dict[str, Any]] = []
            for p in pages:
                t = p.get("title")
                if not t or t == title:
                    continue
                related.append({
                    "title": t,
                    "description": p.get("description"),
                    "url": p.get("fullurl") or f"https://en.wikipedia.org/wiki/{t.replace(' ', '_')}",
                    "thumbnail": (p.get("thumbnail") or {}).get("source"),
                })
            # The generator order is already relevance-sorted; truncate just in case
            return related[:limit]
        except Exception:
            return []

    # ---------------- topics ----------------
    def get_topics(
        self,
        *,
        page: int = 1,
        per_page: int = 30,
        query: Optional[str] = None,
        project: str = "en.wikipedia",      # NOTE: keep without ".org" for pageviews endpoint
        access: str = "all-access",
        **_: Any
    ) -> Dict[str, Any]:
        assert page >= 1 and per_page >= 1

        topics: List[Dict[str, Any]] = []
        has_more = False

        if query:
            res = self._mw_get({
                "action": "query",
                "list": "search",
                "srlimit": min(per_page, 50),
                "srsearch": query,
            })
            hits = (res.get("query") or {}).get("search") or []
            for h in hits:
                title = h.get("title")
                snippet = _strip_html(h.get("snippet"))
                topics.append({
                    "type": "article",
                    "input_identification": title,
                    "title": title,
                    "description": snippet or None,
                    "url": f"https://en.wikipedia.org/wiki/{title.replace(' ', '_')}",
                    "score": h.get("score"),
                })
            return {
                "topics": topics,
                "page": page,
                "per_page": per_page,
                "has_more": False,
                "updated_at": _now_iso(),
                "item_name": self.item_name,
                "source_name": self.source_name,
            }

        # No query -> try Pageviews Top (yesterday UTC)
        yd = _yesterday_utc()
        url = PAGEVIEWS_TOP.format(
            project=project,
            access=access,
            year=str(yd.year),
            month=f"{yd.month:02d}",
            day=f"{yd.day:02d}",
        )

        items: List[Dict[str, Any]] = []
        try:
            data = self._rest_get(url)
            items = ((data.get("items") or [])[0] or {}).get("articles") or []
        except requests.HTTPError as e:
            status = getattr(e.response, "status_code", None)
            # Graceful fallbacks for 403/429/5xx or empty data
            if status in (403, 429, 500, 502, 503, 504):
                # small backoff on 429
                if status == 429:
                    time.sleep(1.0)
                # Featured feed "mostread"
                feed_url = FEED_FEATURED.format(
                    year=str(yd.year), month=f"{yd.month:02d}", day=f"{yd.day:02d}"
                )
                try:
                    feed = self._rest_get(feed_url)
                    mostread = (feed.get("mostread") or {}).get("articles") or []
                    # Normalize to same shape as pageviews top
                    items = [{"article": a.get("normalizedtitle") or a.get("title"),
                              "views": a.get("views", 0)} for a in mostread if a.get("title")]
                except Exception:
                    items = []
            else:
                # re-raise unknown errors
                raise

        # Paginate over the list (whether pageviews or mostread)
        start, end = (page - 1) * per_page, (page - 1) * per_page + per_page
        slice_ = items[start:end]
        for it in slice_:
            raw = it.get("article") or ""
            if raw in ("Main_Page", "-", ""):
                continue
            title_disp = raw.replace("_", " ")
            topics.append({
                "type": "article",
                "input_identification": title_disp,
                "title": title_disp,
                "description": (f"Top viewed yesterday • {it.get('views', 0)} views"
                                if it.get("views") is not None else None),
                "url": f"https://en.wikipedia.org/wiki/{raw or title_disp.replace(' ', '_')}",
                "views": it.get("views"),
            })
        has_more = end < len(items)

        return {
            "topics": topics,
            "page": page,
            "per_page": per_page,
            "has_more": has_more,
            "updated_at": _now_iso(),
            "item_name": self.item_name,
            "source_name": self.source_name,
        }

    # ---------------- input ----------------
    def get_input(
        self,
        *,
        input_identification: str | None = None,   # preferred: page title (normalized, spaces ok)
        permalink_or_url: str | None = None,
        include_sections: bool = True,
        include_categories: bool = True,
        include_related: bool = True,
        thumb_size: int = 480,
    ) -> Dict[str, Any]:
        """
        Build a canonical article bundle:
          {
            "input_identification": "<Title>",
            "input_data": {
              "page": { pageid, title, description, lang, url, extract, thumbnail },
              "sections": [ {index, line, anchor} ],
              "categories": [ "Category:..." ],
              "related": [ {title, description, url, thumbnail} ]
            },
            "updated_at": "..."
          }
        """
        title = (input_identification or "").strip() if input_identification else None
        if not title and permalink_or_url:
            title = self._title_from_url(permalink_or_url)

        if not title:
            return {"error": "missing input_identification and permalink_or_url"}

        # 1) REST summary (fast, language-agnostic metadata)
        sum_data = self._rest_get(REST_SUMMARY.format(title=title.replace(" ", "_")))
        # Normalize basic fields
        pageid = sum_data.get("pageid")
        norm_title = sum_data.get("title") or title
        description = sum_data.get("description")
        lang = sum_data.get("lang") or "en"
        canonicalurl = sum_data.get("content_urls", {}).get("desktop", {}).get("page") or f"https://en.wikipedia.org/wiki/{norm_title.replace(' ', '_')}"
        thumbnail = (sum_data.get("thumbnail") or {}).get("source")

        # 2) Lead extract & thumbnail via action=query (for plaintext + guaranteed thumb)
        q_params = {
            "action": "query",
            "prop": "extracts|pageimages",
            "explaintext": 1,
            "exintro": 1,
            "titles": norm_title,
            "piprop": "thumbnail|name",
            "pithumbsize": thumb_size,
        }
        q_data = self._mw_get(q_params)
        q_pages = (q_data.get("query") or {}).get("pages") or []
        extract = None
        if q_pages:
            p0 = q_pages[0]
            extract = p0.get("extract") or extract
            # prefer p0 thumbnail if REST lacked one
            if not thumbnail:
                tn = (p0.get("thumbnail") or {}).get("source")
                if tn:
                    thumbnail = tn

        # 3) Sections (parse API)
        sections: List[Dict[str, Any]] = []
        if include_sections:
            sec_res = self._mw_get({
                "action": "parse",
                "page": norm_title,
                "prop": "sections",
                "disablelimitreport": 1,
            })
            for s in (sec_res.get("parse") or {}).get("sections") or []:
                sections.append({
                    "index": s.get("index"),
                    "line": s.get("line"),
                    "anchor": s.get("anchor"),
                })

        # 4) Categories
        categories: List[str] = []
        if include_categories:
            cat_res = self._mw_get({
                "action": "query",
                "prop": "categories",
                "cllimit": 50,
                "titles": norm_title,
            })
            for p in (cat_res.get("query") or {}).get("pages") or []:
                for c in (p.get("categories") or []):
                    name = c.get("title")
                    if name:
                        categories.append(name)

        # 5) Related (REST)
        related: List[Dict[str, Any]] = []
        if include_related:
            related = self._fetch_related_pages(norm_title, limit=10, thumb_size=thumb_size)

        return {
            "input_identification": norm_title,
            "input_data": {
                "page": {
                    "pageid": pageid,
                    "title": norm_title,
                    "description": description,
                    "lang": lang,
                    "url": canonicalurl,
                    "extract": extract,
                    "thumbnail": thumbnail,
                },
                "sections": sections,
                "categories": categories,
                "related": related,
            },
            "updated_at": _now_iso(),
        }

    # ---------------- context ----------------
    def generate_context(self, input_data: Dict[str, Any], amount_question: int = 10) -> str:
        page = (input_data or {}).get("page", {}) or {}
        sections = (input_data or {}).get("sections", []) or []
        related = (input_data or {}).get("related", []) or []

        title = page.get("title", "Unknown")
        desc = page.get("description") or ""
        extract = page.get("extract") or ""

        context = []
        context.append(f"Wikipedia article title: {title}")
        if desc:
            context.append(f"Short description: {desc}")
        context.append("")
        context.append("Lead extract:")
        context.append(_strip_html(extract) or "[no text]")
        context.append("")

        if sections:
            context.append("Key sections:")
            # list a handful to keep it compact
            for s in sections[:8]:
                line = s.get("line") or ""
                context.append(f"- {line}")
            context.append("")

        if related:
            context.append("Related pages:")
            for r in related[:6]:
                rtitle = r.get("title")
                rdesc = r.get("description")
                if rtitle:
                    context.append(f"- {rtitle}" + (f": {rdesc}" if rdesc else ""))
            context.append("")

        context.append(self.context_output_structure(amount_question=amount_question))
        return "\n".join(context)
    
    
