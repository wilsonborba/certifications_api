# src/dal/remote/companies_marketcap_adapter.py
from __future__ import annotations

from typing import Any, Dict, List, Optional
from datetime import datetime, timezone
from urllib.parse import urljoin
import requests
from bs4 import BeautifulSoup

from src.dal.remote.base import BaseAdapter
from src.domain.models.preview_model import PreviewModel, EnumMode

BASE = "https://companiesmarketcap.com"

class CompaniesMarketCapAdapter(BaseAdapter):
    item_name = "companies_marketcap"
    source_name = "apps"

    # ---------- Preview ----------
    def get_preview(self) -> PreviewModel:
        return PreviewModel(
            mode=EnumMode.BOTH,
            source_name=self.source_name,
            has_topic=True,
            item_name=self.item_name,
            item_img="https://res.cloudinary.com/dhncdmb2t/image/upload/v1756539100/companies_marketcap_icon_k7e8rp.png",
            updated_at=datetime.now(timezone.utc).isoformat(),
        )

    # ---------- HTTP ----------
    def _get_html(self, url: str, params: Optional[Dict[str, Any]] = None) -> BeautifulSoup:
        r = requests.get(
            url,
            params=params or {},
            timeout=20,
            headers={"User-Agent": "quiz-certify/1.0"}
        )
        r.raise_for_status()
        return BeautifulSoup(r.text, "html.parser")

    # ---------- Parsing ----------
    def _extract_companies(self, soup: BeautifulSoup) -> List[Dict[str, str]]:
        """
        Extract ONLY company name and logo URL from the list table.
        Looks for:
          <td class="name-td">
            <div class="logo-container"><img class="company-logo" ... src="/img/company-logos/64/NVDA.png"></div>
            <div class="name-div"><a ...><div class="company-name">NVIDIA</div> ... </a></div>
          </td>
        """
        topics: List[Dict[str, str]] = []
        for td in soup.select("td.name-td"):
            name_el = td.select_one(".company-name")
            logo_el = td.select_one("img.company-logo")
            if not name_el or not logo_el:
                continue

            name = name_el.get_text(strip=True)
            logo_src = logo_el.get("src") or ""
            if not name or not logo_src:
                continue

            # make logo absolute
            logo_url = urljoin(BASE, logo_src)
            topics.append({"name": name, "logo": logo_url})

        return topics

    # ---------- Public: unified Topics ----------
    def get_topics(
        self,
        *,
        page: int = 1,
        per_page: int = 30,
        **_: Any
    ) -> Dict[str, Any]:
        assert page >= 1 and per_page >= 1

        url = f"{BASE}/page/{page}/"
        soup = self._get_html(url)

        all_companies = self._extract_companies(soup)
        topics = all_companies[:per_page]

        # heuristic has_more: if we saw at least per_page items, assume there could be more
        has_more = len(all_companies) >= per_page

        return {
            "topics": topics,                   # e.g. [{ "name": "NVIDIA", "logo": "https://..." }, ...]
            "page": page,
            "per_page": per_page,
            "has_more": has_more,
            "fetched_at": datetime.now(timezone.utc).isoformat(),
            "item_name": self.item_name,
            "source_name": self.source_name,
        }
