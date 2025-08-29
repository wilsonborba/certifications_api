# src/dal/remote/aws_whitepaper_services_adapter.py
from __future__ import annotations

from typing import Any, Dict, List, Optional, Tuple
from datetime import datetime, timezone
from urllib.parse import urljoin, urlparse
import re
import requests
from bs4 import BeautifulSoup

from src.dal.remote.base import BaseAdapter
from src.domain.models.preview_model import PreviewModel, EnumMode

BASE = "https://docs.aws.amazon.com/whitepapers/latest/aws-overview/"
OVERVIEW = urljoin(BASE, "amazon-web-services-cloud-platform.html")

class AwsWhitepaperServicesAdapter(BaseAdapter):
    """
    Topics = pairs of { service_category, service } from AWS whitepaper pages.

    Flow:
      1) Parse overview page for category links (e.g., ./analytics.html)
      2) For each category page, extract services listed under:
         <div class="highlights" id="inline-topiclist"> ... <ul><li><a>Service</a></li> ...
    Output (TopicsModel fields):
      - topics: [{ "service_category": str, "service": str }, ...]
      - page, per_page, has_more, fetched_at, item_name, source_name
    """
    item_name = "aws_whitepaper_services"
    source_name = "apps"

    # ---------- Preview ----------
    def get_preview(self) -> PreviewModel:
        return PreviewModel(
            mode=EnumMode.BOTH,
            source_name=self.source_name,
            has_topic=True,
            item_name=self.item_name,
            item_img="https://res.cloudinary.com/dhncdmb2t/image/upload/v1756380014/aws_odvewo.png",
            updated_at=datetime.now(timezone.utc).isoformat(),
        )

    # ---------- HTTP ----------
    def _get_html(self, url: str) -> BeautifulSoup:
        r = requests.get(url, timeout=25, headers={"User-Agent": "quiz-certify/1.0"})
        r.raise_for_status()
        return BeautifulSoup(r.text, "html.parser")

    # ---------- Parsing helpers ----------
    _WS = re.compile(r"\s+")

    def _clean(self, s: Optional[str]) -> str:
        return self._WS.sub(" ", (s or "").strip())

    def _extract_categories(self, soup: BeautifulSoup) -> List[Tuple[str, str]]:
        """
        From the overview page, get (category_name, absolute_url) pairs.
        The grid lives inside .table-container; category links are <a href="./<slug>.html">Name</a>
        """
        cats: List[Tuple[str, str]] = []
        for a in soup.select(".table-container a[href$='.html']"):
            name = self._clean(a.get_text())
            href = a.get("href") or ""
            # Keep only first-level category pages (ignore anchors like '#...')
            if not href or href.startswith("#"):
                continue
            url = urljoin(OVERVIEW, href)
            # Heuristic: ensure it points within the whitepaper folder
            if url.startswith(BASE) and url.endswith(".html"):
                if name and (name, url) not in cats:
                    cats.append((name, url))
        return cats

    def _extract_services_from_category(self, category_name: str, category_url: str) -> List[Dict[str, str]]:
        """
        On category page, find div.highlights#inline-topiclist and collect services from its <ul><li><a>...</a>
        Returns list of { service_category, service } dicts.
        """
        soup = self._get_html(category_url)

        block = soup.select_one('div.highlights#inline-topiclist')
        if not block:
            return []

        services: List[Dict[str, str]] = []
        for a in block.select("ul li a"):
            svc = self._clean(a.get_text())
            if not svc:
                continue
            services.append({"service_category": category_name, "service": svc})
        return services

    # ---------- Public: unified Topics ----------
    def get_topics(
        self,
        *,
        page: int = 1,
        per_page: int = 60,
        # You may restrict which categories to crawl (names or partials). Leave None for all.
        include_categories: Optional[List[str]] = None,
        **_: Any,
    ) -> Dict[str, Any]:
        assert page >= 1 and per_page >= 1

        # 1) Categories from overview
        overview_soup = self._get_html(OVERVIEW)
        categories = self._extract_categories(overview_soup)

        # Optional filter by include_categories (case-insensitive substring match)
        if include_categories:
            want = [s.lower() for s in include_categories]
            categories = [(n, u) for (n, u) in categories if any(w in n.lower() for w in want)]

        # 2) Gather services for each category
        all_pairs: List[Dict[str, str]] = []
        for cat_name, cat_url in categories:
            all_pairs.extend(self._extract_services_from_category(cat_name, cat_url))

        # 3) Numeric pagination over the aggregated list
        start = (page - 1) * per_page
        end = start + per_page
        topics = all_pairs[start:end]
        has_more = end < len(all_pairs)

        return {
            "topics": topics,  # [{ "service_category": "Analytics", "service": "Amazon Athena" }, ...]
            "page": page,
            "per_page": per_page,
            "has_more": has_more,
            "fetched_at": datetime.now(timezone.utc).isoformat(),
            "item_name": self.item_name,
            "source_name": self.source_name,
        }
