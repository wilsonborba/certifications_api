# src/dal/remote/wikipedia_adapter.py
from __future__ import annotations

from time import time
from typing import Any, Dict, List, Optional, Tuple
from datetime import datetime, timezone, timedelta
from urllib.parse import urlparse, unquote
import re
import requests
import time as _time

from src.dal.remote.base import BaseAdapter
from src.domain.models.preview_model import PreviewModel, EnumMode
from src.domain.models.indentifications_model import IdentificationsModel  # <-- added

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
                if not title:
                    continue
                snippet = _strip_html(h.get("snippet"))
                link = f"https://en.wikipedia.org/wiki/{title.replace(' ', '_')}"
                topics.append({
                    "type": "article",
                    "title": title,
                    "description": snippet or None,
                    "url": link,
                    # NEW: identifications replaces input_identification
                    "identifications": IdentificationsModel(
                        input_identification=title,
                        title_identification=snippet,
                        link_identification=link,
                        img_link_identification=None,
                    ),
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
                if status == 429:
                    _time.sleep(1.0)  # small backoff
                # Featured feed "mostread"
                feed_url = FEED_FEATURED.format(
                    year=str(yd.year), month=f"{yd.month:02d}", day=f"{yd.day:02d}"
                )
                try:
                    feed = self._rest_get(feed_url)
                    mostread = (feed.get("mostread") or {}).get("articles") or []
                    items = [{"article": a.get("normalizedtitle") or a.get("title"),
                              "views": a.get("views", 0)} for a in mostread if a.get("title")]
                except Exception:
                    items = []
            else:
                raise
        except Exception:
            items = []

        # Paginate over the list (whether pageviews or mostread)
        start, end = (page - 1) * per_page, (page - 1) * per_page + per_page
        slice_ = items[start:end]
        for it in slice_:
            raw = it.get("article") or ""
            if raw in ("Main_Page", "-", ""):
                continue
            title_disp = raw.replace("_", " ")
            link = f"https://en.wikipedia.org/wiki/{raw or title_disp.replace(' ', '_')}"

            description = (f"Top viewed yesterday • {it.get('views', 0)} views"
                                if it.get("views") is not None else None)

            topics.append({
                "type": "article",
                "title": title_disp,
                "description": description,
                "url": link,
                "views": it.get("views"),
                # NEW: identifications replaces input_identification
                "identifications": IdentificationsModel(
                    input_identification=title_disp,
                    title_identification=description,
                    link_identification=link,
                    img_link_identification=None,
                ),
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
        *args: Any,
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
            "identifications": IdentificationsModel(...),
            "input_data": {
              "page": { pageid, title, description, lang, url, extract, thumbnail },
              "sections": [ {index, line, anchor} ],
              "categories": [ "Category:..." ],
              "related": [ {title, description, url, thumbnail} ]
            },
            "updated_at": "..."
          }
          On error, input_data is {} (empty).
        """
        title = (input_identification or "").strip() if input_identification else None
        if not title and permalink_or_url:
            title = self._title_from_url(permalink_or_url)

        if not title:
            # ERROR: missing identifiers -> empty input_data
            return {
                "identifications": IdentificationsModel(
                    input_identification=None,
                    title_identification=None,
                    link_identification=None,
                    img_link_identification=None,
                ),
                "input_data": {},
                "updated_at": _now_iso(),
            }

        # Try fetching; if anything fails, return empty input_data but keep identifications
        norm_title = title
        canonicalurl = f"https://en.wikipedia.org/wiki/{title.replace(' ', '_')}"
        thumbnail = None
        pageid = None
        description = None
        lang = "en"
        extract = None
        sections: List[Dict[str, Any]] = []
        categories: List[str] = []
        related: List[Dict[str, Any]] = []

        try:
            # 1) REST summary
            sum_data = self._rest_get(REST_SUMMARY.format(title=title.replace(" ", "_")))
            pageid = sum_data.get("pageid")
            norm_title = sum_data.get("title") or title
            description = sum_data.get("description")
            lang = sum_data.get("lang") or "en"
            canonicalurl = sum_data.get("content_urls", {}).get("desktop", {}).get("page") or canonicalurl
            thumbnail = (sum_data.get("thumbnail") or {}).get("source")

            # 2) Lead extract & thumbnail via action=query
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
            if q_pages:
                p0 = q_pages[0]
                extract = p0.get("extract") or extract
                if not thumbnail:
                    tn = (p0.get("thumbnail") or {}).get("source")
                    if tn:
                        thumbnail = tn

            # 3) Sections
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

            # 5) Related
            if include_related:
                related = self._fetch_related_pages(norm_title, limit=10, thumb_size=thumb_size)

            input_data = {
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
            }

            return {
                "identifications": IdentificationsModel(
                    input_identification=norm_title,
                    title_identification=description,
                    link_identification=canonicalurl,
                    img_link_identification=thumbnail,
                ),
                "input_data": input_data,
                "updated_at": _now_iso(),
            }

        except Exception:
            # Any fetch/parse error -> empty input_data, but keep best-effort identifications
            return {
                "identifications": IdentificationsModel(
                    input_identification=norm_title or title,
                    title_identification=description,
                    link_identification=canonicalurl,
                    img_link_identification=thumbnail,
                ),
                "input_data": {},  # <- EMPTY on error
                "updated_at": _now_iso(),
            }
        
        # ---------------- search ----------------
    def search(
        self,
        *args: Any,
        q: str,
        page: int = 1,
        per_page: int = 30,
        mode: str = "api",                 # "api" | "fuzzy" | "fulltext"(alias of "api")
        min_fuzzy: float = 0.60,           # only used when mode="fuzzy" (post-filter threshold)
        namespace: int = 0,                # 0 = main/article
        **kwargs: Any,
    ) -> Dict[str, Any]:
        """
        Wikipedia search with paging, optional fuzzy rerank.

        - mode="api": direct MediaWiki 'search' (best relevance) with paging.
        - mode="fuzzy": fetch via API, then rerank using BaseAdapter._simple_fuzzy_score
                        over title+snippet; also tries 'prefixsearch' fallback if needed.
        - mode="fulltext": alias of "api".
        Returns the same envelope as get_topics().
        """
        assert page >= 1 and per_page >= 1
        q_raw = (q or "").strip()
        if not q_raw:
            return {
                "topics": [],
                "page": page,
                "per_page": per_page,
                "has_more": False,
                "updated_at": _now_iso(),
                "item_name": self.item_name,
                "source_name": self.source_name,
            }

        mode = mode or "api"
        if mode == "fulltext":
            mode = "api"

        # -- primary: list=search (supports sroffset for paging)
        sroffset = (page - 1) * per_page
        try:
            res = self._mw_get({
                "action": "query",
                "list": "search",
                "srsearch": q_raw,
                "srlimit": min(per_page, 50),
                "sroffset": sroffset,
                "srnamespace": namespace,
            })
            hits = (res.get("query") or {}).get("search") or []
            total_hits = int((res.get("query") or {}).get("searchinfo", {}).get("totalhits", 0))
        except Exception:
            hits, total_hits = [], 0

        # -- fallback if nothing found: list=prefixsearch (prefix suggester)
        fallback_used = False
        if not hits:
            try:
                pref = self._mw_get({
                    "action": "query",
                    "list": "prefixsearch",
                    "pssearch": q_raw,
                    "pslimit": min(per_page, 50),
                    "psoffset": sroffset,
                    "psnamespace": namespace,
                })
                hits = (pref.get("query") or {}).get("prefixsearch") or []
                total_hits = len(hits) + sroffset  # best-effort
                fallback_used = True
            except Exception:
                hits, total_hits = [], 0

        # Normalize hits -> (title, snippet, score_like)
        items: List[Tuple[float, Dict[str, Any]]] = []
        for h in hits:
            title = h.get("title")
            if not title:
                continue
            snippet = _strip_html(h.get("snippet") or h.get("matched") or "")
            link = f"https://en.wikipedia.org/wiki/{title.replace(' ', '_')}"
            base_score = float(h.get("score") or 0.0)

            # Optional fuzzy post-score (title + snippet)
            if mode == "fuzzy":
                hay = f"{title} {snippet}".casefold()
                s = self._simple_fuzzy_score(hay, q_raw.casefold())
                if s < min_fuzzy:
                    continue
                score = base_score + (s * 100.0)  # blend, keep API order meaningful
            else:
                score = base_score

            items.append((score, {
                "type": "article",
                "title": title,
                "description": snippet or None,
                "url": link,
                "identifications": IdentificationsModel(
                    input_identification=title,
                    title_identification=snippet or title,
                    link_identification=link,
                    img_link_identification=None,
                ),
                "score": score,
                "fallback": fallback_used,
            }))

        # Sort (desc by score), then stable by title
        items.sort(key=lambda t: (-t[0], t[1]["title"].lower()))
        topics = [it for _, it in items[:per_page]]

        has_more = (page * per_page) < max(total_hits, len(items))
        return {
            "topics": topics,
            "page": page,
            "per_page": per_page,
            "has_more": has_more,
            "updated_at": _now_iso(),
            "item_name": self.item_name,
            "source_name": self.source_name,
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
