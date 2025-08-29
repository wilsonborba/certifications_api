# src/dal/remote/devto_adapter.py
from __future__ import annotations

from typing import Any, Dict, List, Optional
from datetime import datetime, timezone
import requests
from bs4 import BeautifulSoup

from src.dal.remote.base import BaseAdapter
from src.domain.models.preview_model import PreviewModel, EnumMode

DEVTO_BASE = "https://dev.to"
DEVTO_TAGS = f"{DEVTO_BASE}/tags"         # ?page=N
DEVTO_FEED_GLOBAL = f"{DEVTO_BASE}/feed"  # (for later context)
UA = "quiz-certify/1.0 (+https://asodya.com)"

class DevToAdapter(BaseAdapter):
    """
    Topics = Dev.to tags (name + description).
    Pagination: ?page=<n> on /tags.
    """
    item_name = "devto"
    source_name = "apps"

    # ---------- Preview ----------
    def get_preview(self) -> PreviewModel:
        return PreviewModel(
            mode=EnumMode.PLAYFUL,           # fits your Apps/Playful side
            source_name=self.source_name,
            has_topic=True,
            item_name=self.item_name,
            item_img="https://res.cloudinary.com/dhncdmb2t/image/upload/v1756446939/dev_to_vpmmrf.png",
            updated_at=datetime.now(timezone.utc).isoformat(),
        )

    # ---------- HTTP ----------
    def _get_html(self, url: str, params: Optional[Dict[str, Any]] = None) -> BeautifulSoup:
        r = requests.get(url, params=params or {}, timeout=20, headers={"User-Agent": UA})
        r.raise_for_status()
        return BeautifulSoup(r.text, "html.parser")

    # ---------- Parsing helpers ----------
    def _parse_tag_cards(self, soup: BeautifulSoup) -> List[Dict[str, Any]]:
        """
        Dev.to tag index uses 'tag-card' containers.
        We extract:
          - tag slug (from /t/<slug>)
          - display name (text around the link)
          - description (short paragraph)
        """
        topics: List[Dict[str, Any]] = []

        # Robust selectors: dev.to may A/B test classes. We prioritize known ones.
        cards = soup.select("div.tag-card") or soup.select("[data-testid='tag-card']") or soup.select("li.tag-card")
        if not cards:
            # Fallback: grab any block with link to /t/<slug>
            candidates = soup.select("a[href^='/t/']")
            seen = set()
            for a in candidates:
                href = a.get("href", "")
                if not href.startswith("/t/"):
                    continue
                slug = href.split("/t/", 1)[-1].strip("/").split("?")[0]
                if not slug or slug in seen:
                    continue
                seen.add(slug)
                # name = prefer visible text near the link
                name = (a.get_text(strip=True) or slug).strip()
                # description: look around for a <p> sibling/parent-desc
                desc_el = a.find_next("p")
                desc = (desc_el.get_text(" ", strip=True) if desc_el else "") or None
                topics.append({"type": "tag", "name": name, "slug": slug, "description": desc})
            return topics

        for card in cards:
            # main link
            a = card.select_one("a[href^='/t/']") or card.find("a")
            if not a:
                continue
            href = a.get("href", "")
            if "/t/" not in href:
                continue
            slug = href.split("/t/", 1)[-1].strip("/").split("?")[0]

            # name: try common headings/text
            name_el = (
                card.select_one("h3") or
                card.select_one("h2") or
                card.select_one(".crayons-tag__name") or
                a
            )
            name = (name_el.get_text(" ", strip=True) if name_el else slug).strip()

            # description: typical selector is a <p> inside card
            desc_el = card.select_one("p")
            desc = (desc_el.get_text(" ", strip=True) if desc_el else "") or None

            topics.append({"type": "tag", "name": name, "slug": slug, "description": desc})

        return topics

    # ---------- Public: Topics (tags only) ----------
    def get_topics(
        self,
        *,
        page: int = 1,
        per_page: int = 45,   # the page itself is already paginated; we’ll cap to per_page
        **_: Any
    ) -> Dict[str, Any]:
        assert page >= 1 and per_page >= 1

        soup = self._get_html(DEVTO_TAGS, params={"page": page})
        tags = self._parse_tag_cards(soup)

        # dev.to already paginates; just cap to per_page
        topics = []
        for t in tags[:per_page]:
            # As requested: only **name + description** (we keep slug internally in case you want it)
            topics.append({
                "name": t["name"],
                "description": t.get("description"),
                # internal fields you might keep or drop:
                # "slug": t["slug"],
                # "type": "tag",
            })

        # basic has_more: if we got any tags AND count >= per_page, assume there might be a next page
        has_more = bool(tags) and (len(tags) >= per_page)

        return {
            "topics": topics,
            "page": page,
            "per_page": per_page,
            "has_more": has_more,
            "fetched_at": datetime.now(timezone.utc).isoformat(),
            "item_name": self.item_name,
            "source_name": self.source_name,
        }

    # ---------- Optional (for later): RSS helpers for context ----------
    # Keep these as utilities to build rich context when a user picks a tag.
    # from: https://dev.to/feed/tag/{tag}, https://dev.to/feed, https://dev.to/feed/{username}
    # You can wire them into a get_context(tag=...) method later.

