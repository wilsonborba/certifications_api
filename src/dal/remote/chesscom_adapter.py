# src/dal/remote/chess_adapter.py
from __future__ import annotations

from typing import Any, Dict, List, Optional, Tuple
from datetime import datetime, timezone
import requests
from bs4 import BeautifulSoup

from src.dal.remote.base import BaseAdapter
from src.domain.models.preview_model import PreviewModel, EnumMode

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
                players.append({
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
                })
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

            news.append({
                "type": "news",
                "title": title,
                "url": url,
                "author": author,
                "published": published_iso,
                "image": img,
            })

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

                news.append({
                    "type": "news",
                    "title": title,
                    "url": url,
                    "author": author,
                    "published": published_iso,
                    "image": img,
                })

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
