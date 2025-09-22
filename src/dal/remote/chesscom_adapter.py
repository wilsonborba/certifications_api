# src/dal/remote/chess_adapter.py
from __future__ import annotations

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
            "• Avoid: speculation about future events, personal judgments, sensitive private info, or unverifiable claims.\n"
            "• Keep wording clear, neutral, and concise; each question should be answerable using the provided context."
        )

    # ---------- Generate textual context for the quiz model ----------
    def generate_context(self, input_data: Dict[str, Any], amount_question: int = 10) -> str:
        """
        Create a compact, readable text context from the item returned by get_input(),
        then append the required context_output_structure(amount_question=...).
        """
        t = (input_data or {}).get("type") or (input_data.get("post", {}) or {}).get("type")  # safety
        lines: list[str] = []

        if t == "news":
            title = input_data.get("title")
            url = input_data.get("url")
            author = input_data.get("author")
            published = input_data.get("published")
            hero = input_data.get("hero_image")
            key_points = input_data.get("key_points") or []

            lines.append("Chess.com News Context")
            if title:     lines.append(f"Title: {title}")
            if author:    lines.append(f"Author: {author}")
            if published: lines.append(f"Published: {published}")
            if url:       lines.append(f"URL: {url}")
            if hero:      lines.append(f"Image: {hero}")
            lines.append("")
            lines.append("Key points (verbatim facts from the article):")
            for p in key_points:
                lines.append(f"- {p}")

        elif t == "player":
            username = input_data.get("username")
            title = input_data.get("title")
            name = input_data.get("name")
            status = input_data.get("status")
            country = input_data.get("country")
            joined = input_data.get("joined")
            last_online = input_data.get("last_online")
            url = input_data.get("url")
            fide = input_data.get("fide")
            ratings = input_data.get("ratings") or {}

            def _fmt_rate(lbl: str, r: dict | None) -> str:
                if not isinstance(r, dict):
                    return f"{lbl}: n/a"
                last = r.get("last")
                best = r.get("best")
                wl = []
                if r.get("wins") is not None:   wl.append(f"W {r.get('wins')}")
                if r.get("losses") is not None: wl.append(f"L {r.get('losses')}")
                if r.get("draws") is not None:  wl.append(f"D {r.get('draws')}")
                record = f" ({', '.join(wl)})" if wl else ""
                last_s = f"{last}" if last is not None else "n/a"
                best_s = f"{best}" if best is not None else "n/a"
                return f"{lbl}: last {last_s}, best {best_s}{record}"

            lines.append("Chess.com Player Context")
            lines.append(f"Username: {username or 'n/a'}")
            if name:      lines.append(f"Name: {name}")
            if title:     lines.append(f"Title: {title}")
            if fide:      lines.append(f"FIDE: {fide}")
            if country:   lines.append(f"Country: {country}")
            if status:    lines.append(f"Account status: {status}")
            if joined:    lines.append(f"Joined (epoch): {joined}")
            if last_online: lines.append(f"Last online (epoch): {last_online}")
            if url:       lines.append(f"Profile: {url}")
            lines.append("")
            lines.append("Ratings snapshot:")
            lines.append(_fmt_rate("Blitz", ratings.get("blitz")))
            lines.append(_fmt_rate("Rapid", ratings.get("rapid")))
            lines.append(_fmt_rate("Bullet", ratings.get("bullet")))
            lines.append(_fmt_rate("Daily", ratings.get("daily")))
            # Optional extras
            tac = ratings.get("tactics")
            if tac:
                lines.append(f"Tactics best: {tac.get('rating', 'n/a')} (score: {tac.get('score', 'n/a')})")
            pr = ratings.get("puzzles_rush")
            if pr:
                lines.append(f"Puzzle Rush best: {pr.get('score', 'n/a')} in {pr.get('total_attempts', 'n/a')} attempts")
        else:
            # Unknown type—print whatever is present to remain helpful
            lines.append("Chess context (untyped)")
            for k, v in (input_data or {}).items():
                lines.append(f"- {k}: {v}")

        lines.append("")
        lines.append("Guidance: Ask about facts explicitly present here (names, dates, results, ratings, key points).")
        lines.append("You may write playful or serious questions, but keep numbers/dates precise and avoid speculation.")

        context = "\n".join(lines)
        context += self.context_output_structure(amount_question=amount_question)
        return context
