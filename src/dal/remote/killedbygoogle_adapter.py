# src/dal/remote/killedbygoogle_adapter.py
from __future__ import annotations

from typing import Any, Dict, List, Optional, Tuple
from datetime import datetime, timezone
from urllib.parse import urljoin
import requests
from bs4 import BeautifulSoup

from src.dal.remote.base import BaseAdapter
from src.domain.models.preview_model import PreviewModel, EnumMode

BASE = "https://killedbygoogle.com"

class KilledByGoogleAdapter(BaseAdapter):
    """
    Topics = discontinued Google products/services from killedbygoogle.com

    Contract:
      returns {
        "topics": [{ "name": "<Product>", "url": "<canonical url>" }, ...],
        "page": int,
        "per_page": int,
        "has_more": bool,
        "fetched_at": iso,
        "item_name": "killed_by_google",
        "source_name": "apps"
      }

    Strategy:
      1) Try JSON endpoints (stable & light):
         - /api/killed.json
         - /graveyard.json
         - /graveyard.min.json
      2) Fallback: scrape HTML and extract product cards.
    """
    item_name = "killed_by_google"
    source_name = "apps"

    # -------- Preview --------
    def get_preview(self) -> PreviewModel:
        return PreviewModel(
            mode=EnumMode.PLAYFUL,
            source_name=self.source_name,
            has_topic=True,
            item_name=self.item_name,
            item_img="https://res.cloudinary.com/dhncdmb2t/image/upload/v1756378449/Screenshot_2025-08-28_175351_stw8ji.png",
            updated_at=datetime.now(timezone.utc).isoformat(),
        )

    # -------- HTTP helpers --------
    def _get_json(self, path: str) -> Optional[List[Dict[str, Any]]]:
        try:
            r = requests.get(urljoin(BASE, path), timeout=20, headers={"User-Agent": "quiz-certify/1.0"})
            if r.status_code == 200 and r.headers.get("content-type", "").startswith("application/json"):
                data = r.json()
                # Some endpoints may return dict with key 'graveyard'
                if isinstance(data, dict) and "graveyard" in data:
                    return data["graveyard"] or []
                return data if isinstance(data, list) else None
        except Exception:
            pass
        return None

    def _get_html(self, path: str = "/") -> Optional[BeautifulSoup]:
        try:
            r = requests.get(urljoin(BASE, path), timeout=20, headers={"User-Agent": "quiz-certify/1.0"})
            r.raise_for_status()
            return BeautifulSoup(r.text, "html.parser")
        except Exception:
            return None

    # -------- JSON normalization --------
    def _topics_from_json(self, items: List[Dict[str, Any]]) -> List[Dict[str, str]]:
        topics: List[Dict[str, str]] = []
        for it in items:
            # Common JSON fields in public datasets:
            # name/title, link (or 'url'/'homepage'), slug
            name = (it.get("name")
                    or it.get("title")
                    or it.get("product")
                    or "").strip()
            if not name:
                continue

            # prefer external link if present, else build a slug URL if provided
            link = (it.get("link")
                    or it.get("url")
                    or it.get("source")
                    or "")
            if link and not link.startswith("http"):
                # sometimes the dataset includes site-relative links; make absolute
                link = urljoin(BASE, link)

            if not link:
                slug = (it.get("slug") or "").strip().strip("/")
                link = urljoin(BASE, f"/{slug}/") if slug else BASE

            topics.append({"name": name, "url": link})
        return topics

    # -------- HTML fallback parsing --------
    def _topics_from_html(self, soup: BeautifulSoup) -> List[Dict[str, str]]:
        """
        Heuristics for the homepage/card grid:
          - cards often contain the product name text and a link to details or an external source
        We keep it conservative: only extract a name + a canonical-ish URL.
        """
        topics: List[Dict[str, str]] = []

        # 1) Try obvious card links (anchor with product name text)
        for a in soup.select("a"):
            text = (a.get_text(" ", strip=True) or "").strip()
            href = a.get("href") or ""
            if not text or len(text) < 2 or not href:
                continue

            # Ignore nav/footers or social links
            if any(x in href for x in ("/privacy", "/about", "twitter.com", "github.com", "mailto:")):
                continue

            # Heuristic: product links are usually within site or external articles
            url = href if href.startswith("http") else urljoin(BASE, href)

            # Be strict to avoid noise: skip anchors whose text is clearly not a product name
            # (short words, non-alphanumeric heavy, etc.)
            if len(text) < 3:
                continue

            topics.append({"name": text, "url": url})

        # 2) Deduplicate by name, keep first URL
        seen = set()
        deduped: List[Dict[str, str]] = []
        for t in topics:
            n = t["name"]
            if n in seen:
                continue
            seen.add(n)
            deduped.append(t)
        return deduped

    # -------- Public: unified Topics --------
    def get_topics(
        self,
        *,
        page: int = 1,
        per_page: int = 30,
        **_: Any
    ) -> Dict[str, Any]:
        assert page >= 1 and per_page >= 1

        # Prefer JSON endpoints
        json_paths = ["/api/killed.json", "/graveyard.json", "/graveyard.min.json"]
        items: Optional[List[Dict[str, Any]]] = None
        for p in json_paths:
            items = self._get_json(p)
            if items:
                break

        if items:
            all_topics = self._topics_from_json(items)
        else:
            # HTML fallback
            soup = self._get_html("/")
            all_topics = self._topics_from_html(soup) if soup else []

        # numeric paging (slice)
        start = (page - 1) * per_page
        end = start + per_page
        topics = all_topics[start:end]
        has_more = end < len(all_topics)

        return {
            "topics": topics,  # [{ "name": "...", "url": "..." }]
            "page": page,
            "per_page": per_page,
            "has_more": has_more,
            "fetched_at": datetime.now(timezone.utc).isoformat(),
            "item_name": self.item_name,
            "source_name": self.source_name,
        }
