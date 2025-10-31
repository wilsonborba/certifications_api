# src/dal/remote/chess_adapter.py
from __future__ import annotations

import json
from typing import Any, Dict, List, Optional, Tuple
from datetime import datetime, timezone
import requests
from bs4 import BeautifulSoup

from src.domain.models.indentifications_model import IdentificationsModel
from src.dal.remote.base import BaseAdapter
from src.domain.models.preview_model import PreviewModel, EnumMode
from src.core.logs import error
from urllib.parse import urlparse

CHESS_API_BASE = "https://api.chess.com/pub"
CHESS_NEWS_BASE = "https://www.chess.com/news"

class ChessComAdapter(BaseAdapter):
    """
    Topics = 50/50 mix of:
      - Chess.com News articles (scraped): newest first, numeric page
      - Leaderboard players from the official API: sliced to fill quota
    Numeric pagination: `page` maps directly to news `?page=<page>`.
    """
    item_name = "chess_com"
    source_name = "apps"



    # ---------- Preview ----------
    def get_preview(self) -> PreviewModel:
        # Keep your preview contract/timestamps
        return PreviewModel(
            mode=EnumMode.PLAYFUL,     # fits your “Apps” playful side; adjust if you want BOTH
            source_name=self.source_name,
            has_topic=True,
            item_name=self.item_name,
            item_img="https://res.cloudinary.com/dhncdmb2t/image/upload/v1756367505/Chess.com_2019__App_Icon_hyanfk.png",
            updated_at=datetime.now(timezone.utc).isoformat(),
        )

    # ---------- HTTP helpers ----------
    def _get_json(self, url: str, params: Optional[Dict[str, Any]] = None) -> Dict[str, Any]:
        r = requests.get(url, params=params or {}, timeout=15, headers={"User-Agent": "quiz-certify/1.0"})
        r.raise_for_status()
        return r.json()

    def _get_html(self, url: str, params: Optional[Dict[str, Any]] = None) -> BeautifulSoup:
        r = requests.get(url, params=params or {}, timeout=15, headers={"User-Agent": "quiz-certify/1.0"})
        r.raise_for_status()
        return BeautifulSoup(r.text, "html.parser")
    
    def _news_id_from_url(self, url: str) -> Optional[str]:
        """
        From https://www.chess.com/news/view/<slug>[?…]
        return '<slug>' as the stable input_identification.
        """
        try:
            p = urlparse(url if url.startswith("http") else f"https://www.chess.com{url}")
            parts = [x for x in p.path.split("/") if x]
            # expect ["news", "view", "<slug>", ...]
            if len(parts) >= 3 and parts[0] == "news" and parts[1] == "view":
                return parts[2]
        except Exception:
            pass
        return None

    # ---------- Official API: leaderboards ----------
    def _fetch_leaderboards(self) -> List[Dict[str, Any]]:
        """
        https://api.chess.com/pub/leaderboards
        We’ll normalize a few categories (e.g., live_blitz) into 'player' topics.
        """
        data = self._get_json(f"{CHESS_API_BASE}/leaderboards")

        # Prefer live blitz; fallback to rapid/bullet/classical if needed
        categories = ["live_blitz", "live_rapid", "live_bullet", "daily", "daily960"]
        players: List[Dict[str, Any]] = []
        for cat in categories:
            lst = data.get(cat) or []
            for p in lst:
            
                t_players = {
                    "type": "player",
                    "category": cat,
                    "username": p.get("username"),
                    "title": p.get("title"),           # e.g., GM/IM/FM
                    "score": p.get("score"),           # leaderboard points
                    "rank": p.get("rank"),
                    "country": p.get("country"),
                    "status": p.get("status"),
                    "icon": p.get("avatar"),
                    "url": p.get("url"),
                }

                t_players['identifications'] = IdentificationsModel(
                    input_identification=p.get("username"),
                    title_identification=f"{p.get('username')} {p.get('title')}",
                    link_identification=p.get("url"),
                    img_link_identification=p.get("avatar"),
                )

                players.append(t_players)
        return players

    # ---------- Scraper: news ----------
    def _fetch_news_page(self, page: int) -> List[Dict[str, Any]]:
        """
        Scrape chess.com/news?page=<page>
        Normalize to 'news' topics (title, url, author, published ISO, image).
        """
        page = max(1, int(page))
        soup = self._get_html(CHESS_NEWS_BASE, params={"page": page})

        def _to_iso(dt_str: Optional[str]) -> Optional[str]:
            if not dt_str:
                return None
            dt_str = dt_str.strip()
            # try ISO-ish first
            try:
                # handles '2025-08-27T23:01' or '2025-08-27 23:01'
                ds = dt_str.replace("T", " ")
                dt = datetime.strptime(ds, "%Y-%m-%d %H:%M")
                # chess.com doesn't include tz; assume UTC for consistency
                return dt.replace(tzinfo=timezone.utc).isoformat()
            except Exception:
                # if parsing fails, just return the raw string
                return dt_str

        news: List[Dict[str, Any]] = []

        # Main card anchors
        for a in soup.select('a.post-preview-component, a[href^="/news/view/"]'):
            href = a.get("href")
            if not href:
                continue
            url = href if href.startswith("http") else f"https://www.chess.com{href}"

            # title
            title_el = a.select_one('[data-testid="post-title"], .post-preview-title, h2, h3')
            title = (title_el.get_text(strip=True) if title_el else "") or a.get("title") or "Chess.com News"

            # preview image (if any)
            img_el = a.select_one("img")
            img = img_el.get("src") if img_el and img_el.get("src") else None

            # --- NEW: meta extraction from .post-preview-meta-component ---
            # try nearest following meta block
            meta = a.find_next("div", class_="post-preview-meta-component")
            if not meta:
                # try within a common container
                container = a.find_parent(lambda t: hasattr(t, "find") and t.find("div", class_="post-preview-meta-component"))
                if container:
                    meta = container.find("div", class_="post-preview-meta-component")

            author = None
            published_iso = None
            if meta:
                author_link = meta.select_one("a.post-preview-meta-username")
                if author_link:
                    # prefer explicit title attr; fallback to text
                    author = author_link.get("title") or author_link.get_text(strip=True)

                time_el = meta.select_one("div.post-preview-meta-time time")
                if time_el:
                    published_iso = _to_iso(time_el.get("datetime") or time_el.get_text(strip=True))

            t_news = {
                "type": "news",

                "title": title,
                "url": url,
                "author": author,
                "published": published_iso,
                "image": img,
            }

            t_news['identifications'] = IdentificationsModel(
                input_identification= self._news_id_from_url(url),
                title_identification=title,
                link_identification=url,
                img_link_identification=img,
            )

            news.append(t_news)

        # Fallback if selector missed newer DOM variants; try generic <article> cards
        if not news:
            for art in soup.select("article"):
                link = art.select_one("a[href]")
                if not link:
                    continue
                href = link.get("href")
                url = href if href.startswith("http") else f"https://www.chess.com{href}"
                title = (art.select_one("h2, h3") or art).get_text(strip=True)
                img_el = art.select_one("img")
                img = img_el.get("src") if img_el and img_el.get("src") else None

                # attempt meta under the article scope too
                meta = art.select_one("div.post-preview-meta-component")
                author = None
                published_iso = None
                if meta:
                    author_link = meta.select_one("a.post-preview-meta-username")
                    if author_link:
                        author = author_link.get("title") or author_link.get_text(strip=True)
                    time_el = meta.select_one("div.post-preview-meta-time time")
                    if time_el:
                        published_iso = _to_iso(time_el.get("datetime") or time_el.get_text(strip=True))


                t_news_2 = {
                    "type": "news",

                    "title": title,
                    "url": url,
                    "author": author,
                    "published": published_iso,
                    "image": img,
                }

                t_news_2['identifications'] = IdentificationsModel(
                    input_identification= self._news_id_from_url(url),
                    title_identification=title,
                    link_identification=url,
                    img_link_identification=img,
                )

                news.append(t_news_2)

        return news

    # ---------- Public: unified topics ----------
    def get_topics(
        self,
        *,
        page: int = 1,
        per_page: int = 30,
        # adapter-specific knobs you might want later:
        leaderboard_category: Optional[str] = None,  # keep for future fine-tuning
        **_: Any
    ) -> Dict[str, Any]:
        """
        Build a numeric topics page with a 50/50 mix:
          - half news from /news?page=<page>
          - half leaderboard players from official API
        """
        assert page >= 1 and per_page >= 1

        # 50/50 split (remainder goes to news)
        half = per_page // 2
        news_quota = half + (per_page % 2)
        players_quota = half

        # Fetch buckets
        news = self._fetch_news_page(page)[:news_quota]

        players = self._fetch_leaderboards()
        # numeric "page" for players: emulate pagination by slicing
        start = (page - 1) * players_quota
        end = start + players_quota
        players_slice = players[start:end]

        # Merge in a predictable order: news first, then players
        topics = (news + players_slice)[:per_page]

        # has_more: if either bucket likely has more
        # - news: assume more if we got any articles (chess.com/news paginates)
        # - players: more if our slice didn't exhaust the list
        news_has_more = len(news) > 0
        players_has_more = end < len(players)
        has_more = news_has_more or players_has_more

        return {
            "topics": topics,
            "page": page,
            "per_page": per_page,
            "has_more": has_more,
            "updated_at": datetime.now(timezone.utc).isoformat(),
            "item_name": self.item_name,
            "source_name": self.source_name,
        }

    # ---------- Input: full item fetch ----------
    def get_input(
        self,
        *,
        input_identification: str | None = None,
        topic_type: str | None = None,
        url: str | None = None,
        username: str | None = None,
        max_paragraphs: int = 18,
        **_: Any,
    ) -> Dict[str, Any]:
        """
        Resolve and return a full item payload for a topic previously listed by `get_topics`.

        Supported IDs:
          - News article: use the article `url` as input_identification (or pass via `url`).
          - Player: use the chess.com `username` as input_identification (or pass via `username`).

        Returns:
          {
            "input_identification": "...",
            "input_data": {...},      # normalized, useful for question writing
            "updated_at": "...iso..."
          }
        """
        now_iso = datetime.now(timezone.utc).isoformat()

        # ---- Resolve identification + type ----
        ident = input_identification

        if topic_type == "news" or (url and "/news" in url) or (ident and isinstance(ident, str) and "/" not in ident and ident.strip()):
            # NEWS branch
            if not url:
                # If we only got the slug (preferred), build the canonical URL
                if ident and "/" not in ident:
                    url = f"{CHESS_NEWS_BASE}/view/{ident}"
                else:
                    url = ident  # ident may already be a full URL

            ident = url
            if not ident:
                error("Missing URL for news article input")
                return {"input_identification": "", "input_data": {}, "updated_at": now_iso}

            try:
                soup = self._get_html(ident)
            except Exception as e:
                error(f"Failed to fetch news article at {ident}: {e}")
                return {"input_identification": ident, "input_data": {}, "updated_at": now_iso}

            # Extract content
            title = None
            h = soup.select_one('[data-testid="post-title"], .post-view-title, h1')
            if h:
                title = h.get_text(strip=True)

            # author
            author = None
            a_el = soup.select_one("a.post-view-meta-username, a.post-preview-meta-username")
            if a_el:
                author = a_el.get("title") or a_el.get_text(strip=True)

            # published
            published = None
            t_el = soup.select_one("div.post-view-meta time, div.post-preview-meta-time time, time[datetime]")
            if t_el:
                published = (t_el.get("datetime") or t_el.get_text(strip=True) or "").strip()

            # hero image (best-effort)
            hero = None
            hero_el = soup.select_one('figure img, .post-view-hero img, img[src*="news"]')
            if hero_el and hero_el.get("src"):
                hero = hero_el["src"]

            # main text content: collect paragraphs and bullet items
            body_blocks: list[str] = []
            # prefer a main content container
            main = soup.select_one(".post-view-component, article, main") or soup
            # common content selectors
            for p in main.select("p"):
                txt = p.get_text(" ", strip=True)
                if txt:
                    body_blocks.append(txt)
            # list items (sometimes key facts are in lists)
            for li in main.select("li"):
                txt = li.get_text(" ", strip=True)
                if txt:
                    body_blocks.append(f"• {txt}")

            # Trim to avoid huge prompts
            if max_paragraphs > 0:
                body_blocks = body_blocks[:max_paragraphs]

            input_data = {
                "type": "news",
                "title": title,
                "url": ident,
                "author": author,
                "published": published,
                "hero_image": hero,
                "key_points": body_blocks,
            }

            input_ = {
                "input_data": input_data,
                "updated_at": now_iso,
            }

            input_['identifications'] = IdentificationsModel(
                input_identification=ident,
                title_identification=title,
                link_identification=ident,
                img_link_identification=hero,
            )
            
            return input_
        
        # If the caller already told us it's a player
        if topic_type == "player":
            username = username or ident
        # Or try to detect from looks (no /news in URL and non-empty -> treat as username)
        if username:
            ident = username

        # PLAYER branch
        if ident:
            # Normalize username casing (Chess.com usernames are case-insensitive)
            uname = (ident or "").strip().lstrip("@")
            if not uname:
                input_2 = { "input_data": {}, "updated_at": now_iso}
                input_2['identifications'] = IdentificationsModel(
                    input_identification="",
                    title_identification=None,
                    link_identification=None,
                    img_link_identification=None,
                )

                return input_2

            profile = None
            stats = None
            country_name = None
            try:
                profile = self._get_json(f"{CHESS_API_BASE}/player/{uname}")
            except Exception as e:
                input_3 = {"updated_at": now_iso}

                input_3['identifications'] = IdentificationsModel(
                    input_identification=uname,
                    title_identification=None,
                    link_identification=None,
                    img_link_identification=None,
                )

                return input_3


            try:
                stats = self._get_json(f"{CHESS_API_BASE}/player/{uname}/stats")
            except Exception:
                stats = {}

            # Optionally resolve country name
            try:
                c_url = (profile or {}).get("country")
                if c_url:
                    c = self._get_json(c_url)
                    country_name = c.get("name")
            except Exception:
                country_name = None

            # Pull friendly ratings snapshot
            def _rate(block: dict | None):
                if not isinstance(block, dict):
                    return None
                last = (block.get("last") or {}).get("rating")
                best = (block.get("best") or {}).get("rating")
                rec = block.get("record") or {}
                return {
                    "last": last,
                    "best": best,
                    "wins": rec.get("win"),
                    "losses": rec.get("loss"),
                    "draws": rec.get("draw"),
                }

            ratings = {
                "blitz": _rate(stats.get("chess_blitz")),
                "rapid": _rate(stats.get("chess_rapid")),
                "bullet": _rate(stats.get("chess_bullet")),
                "daily": _rate(stats.get("chess_daily")),
                "tactics": (stats.get("tactics", {}) or {}).get("highest", {}),
                "puzzles_rush": (stats.get("puzzle_rush", {}) or {}).get("best", {}),
            }

            input_data = {
                "type": "player",
                "username": profile.get("username") or uname,
                "title": profile.get("title"),
                
                "name": profile.get("name"),
                "status": profile.get("status"),
                "country": country_name or profile.get("country"),
                "joined": profile.get("joined"),
                "last_online": profile.get("last_online"),
                "avatar": profile.get("avatar"),
                "url": profile.get("url"),
                "fide": profile.get("fide"),
                "ratings": ratings,
            }

            input_4 = {
                "input_data": input_data,
                "updated_at": now_iso,
            }

            input_4['identifications'] = IdentificationsModel(
                input_identification=uname,
                title_identification=f"{profile.get('username')} {profile.get('title')}",
                link_identification=profile.get("url"),
                img_link_identification=profile.get("avatar"),
            )

            return input_4

        # Fallback (nothing resolved)
        input_5 = {

            "input_data": {},
            "updated_at": now_iso,
        }

        input_5['identifications'] = IdentificationsModel(
            input_identification=input_identification,
            title_identification=None,
            link_identification=None,
            img_link_identification=None,
        )

        return input_5
    
        # ---------- SEARCH (bounded local over News + Players) ----------
    def search(
        self,
        q: str,
        page: int = 1,
        per_page: int = 20,
        mode: str = "fulltext",        # "fulltext" | "substring" | "fuzzy"
        fill_page: bool = True,
        max_extra_pages: int = 2,      # only used to pull more news pages when filter shrinks results
        *args: Any,
        **kwargs: Any,
    ) -> Dict[str, Any]:
        """
        No first-class Chess.com search API exists, so we implement:
          - News: scrape /news?page=N (starting at `page`) → title match; if needed, enrich a bounded set by
                   fetching article bodies and matching there too.
          - Players: pull leaderboards once and match on username/title; optionally enrich a few profiles to
                     match on real 'name' as well.
        Bounded & fast: we only fetch (page + up to max_extra_pages) news pages when needed, and enrich bodies
        for at most ~8 articles + ~12 player profiles total per call.
        """
        assert isinstance(q, str) and q.strip(), "q must be non-empty"
        assert page >= 1 and per_page >= 1
        qn = q.casefold()

        # ------------------- gather candidates -------------------
        # News: start from requested numeric page
        news_items = self._fetch_news_page(page)

        # Players: single call to leaderboards (already bounded)
        players_items = self._fetch_leaderboards()

        # Normalize buckets
        news_norm = [self._normalize_news_topic(x) for x in news_items]
        news_norm = [x for x in news_norm if x is not None]

        players_norm = [self._normalize_player_topic(x) for x in players_items]
        players_norm = [x for x in players_norm if x is not None]

        # ------------------- filtering modes -------------------
        # For adapters without server pushdown, treat "fulltext" as substring on what we already have.
        selected: List[Dict[str, Any]] = []

        # 1) first-pass: title/username on already-fetched lists (cheap)
        if mode in ("fulltext", "substring", "fuzzy"):
            selected.extend(self._filter_news_light(news_norm, q, mode))
            selected.extend(self._filter_players_light(players_norm, q, mode))

        # Need more? Enrich content and re-filter (bounded)
        MIN_OK = min(5, per_page)  # make sure we return something for common queries
        need_more = len(selected) < MIN_OK

        if need_more:
            # a) Pull extra news pages (bounded) and re-apply light filter
            extra_scanned = 0
            next_page = page + 1
            while extra_scanned < max_extra_pages and len(selected) < MIN_OK:
                more_news = self._fetch_news_page(next_page)
                extra_scanned += 1
                next_page += 1
                cand = [self._normalize_news_topic(x) for x in more_news]
                cand = [x for x in cand if x]
                selected.extend(self._filter_news_light(cand, q, mode))

            # b) Enrich a limited number of news items with article body and substring/fuzzy into body
            if len(selected) < MIN_OK:
                selected = self._enrich_and_filter_news_body(selected, q, limit_enrich=8, mode=mode)

            # c) Enrich a limited number of players with profile 'name' and match there too
            if len(selected) < per_page:
                selected = self._enrich_and_filter_player_name(players_norm, q, limit_profiles=12, carry=selected, mode=mode)

        # ------------------- sort & paginate (stable) -------------------
        # Score: prefer news recency + players rank/score; keep deterministic fallback by title
        def _rank_key(it: Dict[str, Any]):
            if it.get("type") == "news":
                # use published (iso) if present
                ts = self._iso_to_ts(it.get("published"))
                return (2, ts or 0, it.get("title") or "")
            else:
                # player: higher score/rank first (rank smaller is better → invert)
                rank = it.get("rank") or 1e9
                score = it.get("score") or 0
                return (1, -score, -1.0 / max(rank, 1), it.get("title") or "")

        # dedupe by ident (news slug URL / username)
        seen = set()
        deduped: List[Dict[str, Any]] = []
        for it in selected:
            ident = it["identifications"].input_identification
            if ident in seen:
                continue
            seen.add(ident)
            deduped.append(it)

        deduped.sort(key=_rank_key, reverse=True)

        # Pagination of the merged result (we already limited sources; this is UI paging)
        start = (page - 1) * per_page
        end = start + per_page
        topics = deduped[start:end]
        has_more = end < len(deduped)

        return {
            "item_name": self.item_name,
            "source_name": self.source_name,
            "page": page,
            "per_page": per_page,
            "has_more": has_more,
            "updated_at": datetime.now(timezone.utc).isoformat(),
            "topics": topics,
        }

    # ---------- normalization ----------
    def _normalize_news_topic(self, t: Dict[str, Any]) -> Optional[Dict[str, Any]]:
        """
        Accepts one element from _fetch_news_page() and ensures IdentificationsModel is present.
        """
        from src.domain.models.indentifications_model import IdentificationsModel
        title = (t.get("title") or "").strip()
        url   = (t.get("url") or "").strip()
        if not title or not url:
            return None
        slug = self._news_id_from_url(url) or url
        ident = IdentificationsModel(
            input_identification=slug,
            title_identification=title,
            link_identification=url,
            img_link_identification=t.get("image"),
        )
        out = dict(t)
        out["identifications"] = ident
        return out

    def _normalize_player_topic(self, t: Dict[str, Any]) -> Optional[Dict[str, Any]]:
        """
        Accepts one element from _fetch_leaderboards() and ensures IdentificationsModel is present.
        """
        from src.domain.models.indentifications_model import IdentificationsModel
        username = (t.get("username") or "").strip()
        url      = (t.get("url") or "").strip()
        if not username or not url:
            return None
        title = (t.get("title") or "")  # GM/IM/FM etc.
        ident = IdentificationsModel(
            input_identification=username,
            title_identification=f"{username} {title}".strip(),
            link_identification=url,
            img_link_identification=t.get("icon"),
        )
        out = dict(t)
        out["identifications"] = ident
        # convenience fields for ranking
        out["rank"] = t.get("rank")
        out["score"] = t.get("score")
        return out

    # ---------- light filters on already-fetched lists ----------
    def _filter_news_light(self, items: List[Dict[str, Any]], q: str, mode: str) -> List[Dict[str, Any]]:
        qn = q.casefold()
        out: List[Dict[str, Any]] = []
        for it in items:
            title = (it.get("title") or "").casefold()
            if mode == "fuzzy":
                if self._simple_fuzzy_score(title, qn) >= 0.78:
                    out.append(it)
            else:  # fulltext/substring
                if qn in title:
                    out.append(it)
        return out

    def _filter_players_light(self, items: List[Dict[str, Any]], q: str, mode: str) -> List[Dict[str, Any]]:
        qn = q.casefold()
        out: List[Dict[str, Any]] = []
        for it in items:
            uname = (it.get("username") or "").casefold()
            title = (it.get("title") or "").casefold()
            combo = f"{uname} {title}"
            if mode == "fuzzy":
                if self._simple_fuzzy_score(combo, qn) >= 0.78:
                    out.append(it)
            else:
                if (qn in uname) or (qn in title):
                    out.append(it)
        return out

    # ---------- enrichment: news body ----------
    def _enrich_and_filter_news_body(
        self,
        selected: List[Dict[str, Any]],
        q: str,
        *,
        limit_enrich: int = 8,
        mode: str = "substring",
    ) -> List[Dict[str, Any]]:
        """
        For up to `limit_enrich` news items that DON'T yet match by title, fetch article page,
        extract text blocks, and match q against body (substring/fuzzy).
        """
        qn = q.casefold()
        keep = list(selected)
        # Choose candidates from recent news not already selected
        # (we’ll just re-fetch current page; caller already extended pages if needed)
        tried = 0
        # Build a small set from 'selected' to avoid duplicates
        already = {it["identifications"].input_identification for it in selected}
        # We can re-use the current page news list quickly
        base_news = self._fetch_news_page(1)  # page 1 is hottest; cheap heuristic
        for raw in base_news:
            if tried >= limit_enrich:
                break
            n = self._normalize_news_topic(raw)
            if not n:
                continue
            ident = n["identifications"].input_identification
            if ident in already:
                continue
            tried += 1

            url = n.get("url")
            try:
                soup = self._get_html(url)
            except Exception:
                continue

            # pull a compact body (similar to get_input)
            main = soup.select_one(".post-view-component, article, main") or soup
            blocks: List[str] = []
            for p in main.select("p"):
                txt = p.get_text(" ", strip=True)
                if txt:
                    blocks.append(txt)
            for li in main.select("li"):
                txt = li.get_text(" ", strip=True)
                if txt:
                    blocks.append(txt)
            body_text = " \n".join(blocks)[:6000]  # cap

            ok = False
            if mode == "fuzzy":
                ok = self._simple_fuzzy_score(body_text.casefold(), qn) >= 0.78
            else:
                ok = qn in body_text.casefold()

            if ok:
                n2 = dict(n)
                n2["body_hit"] = True
                keep.append(n2)

        return keep

    # ---------- enrichment: player real name ----------
    def _enrich_and_filter_player_name(
        self,
        players_norm: List[Dict[str, Any]],
        q: str,
        *,
        limit_profiles: int = 12,
        carry: List[Dict[str, Any]],
        mode: str = "substring",
    ) -> List[Dict[str, Any]]:
        qn = q.casefold()
        keep = list(carry)
        already = {it["identifications"].input_identification for it in carry}

        count = 0
        for it in players_norm:
            if count >= limit_profiles or len(keep) >= 2 * limit_profiles:
                break
            uname = it.get("username")
            if not uname or uname in already:
                continue
            count += 1
            # Fetch profile to get 'name'
            try:
                prof = self._get_json(f"{CHESS_API_BASE}/player/{uname}")
            except Exception:
                continue
            real_name = (prof.get("name") or "").casefold()
            if not real_name:
                continue

            ok = False
            if mode == "fuzzy":
                ok = self._simple_fuzzy_score(real_name, qn) >= 0.78
            else:
                ok = qn in real_name

            if ok:
                keep.append(it)
                already.add(uname)

        return keep


    def _iso_to_ts(self, iso: Optional[str]) -> Optional[float]:
        try:
            if not iso:
                return None
            # Best-effort parsing; chess.com sometimes omits TZ—treat as UTC
            s = iso.replace("T", " ").replace("Z", "")
            dt = datetime.strptime(s, "%Y-%m-%d %H:%M")
            return dt.replace(tzinfo=timezone.utc).timestamp()
        except Exception:
            return None



    # ---------- Instructions: concise & creativity-friendly ----------
    def instructions(self) -> str:
        """
        Short, flexible guidance for writing quiz questions from Chess.com news or player stats.
        Encourages creativity while keeping facts precise and verifiable from the provided context.
        """
        return (
            "You will receive either (A) a Chess.com news article (title, author, published date, key points), "
            "or (B) a player profile with current ratings and records. Write engaging quiz questions that can fit "
            "either playful or serious modes:\n"
            "\n"
            "• Stay factual: numbers, dates, names, results, titles, and ratings must match the context exactly.\n"
            "• Creativity welcome: you may add fun phrasing or analogies, but never change facts.\n"
            "• External knowledge is okay only if it is standard chess knowledge AND clearly consistent with the context; "
            "do not contradict or fill gaps with guesses.\n"
            "• Good question ideas: identify key facts, compare ratings or streaks, ‘which month/score/title’, cause/effect described "
            "in the article, who/what/when/where, ordering (highest→lowest ratings), and short scenario questions grounded in the data.\n"
            "• Keep wording clear, neutral, and concise; each question should be answerable using the provided context."
        )

    # ---------------- context ----------------
    def generate_context(self, input_data: Dict[str, Any], amount_question: int = 10) -> str:
        """
        Builds a plain-text context string combining all key/value pairs in input_data
        and the model output structure, separated by newlines.
        """

        context_lines: list[str] = []

        # Safely iterate key/value pairs — stringify everything
        for key, value in (input_data or {}).items():
            # Represent complex values like dicts/lists in a readable way
            if isinstance(value, (dict, list, tuple, set)):
                context_lines.append(f"{key}: {json.dumps(value, ensure_ascii=False)}")
            else:
                context_lines.append(f"{key}: {value}")

        # Add your output structure
        output_structure = self.context_output_structure(amount_question=amount_question)
        context_lines.append(str(output_structure))

        # Join them all with newline separators
        return "\n".join(context_lines)
