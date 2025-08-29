from __future__ import annotations
from typing import Any, Dict, List, Tuple, Optional
from datetime import datetime, timezone
import re
import random
import requests

from src.dal.remote.base import BaseAdapter
from src.domain.models.preview_model import PreviewModel, EnumMode

# BLS flat file (same path for http/https)
BLS_CU_AREA_PATH = "download.bls.gov/pub/time.series/cu/cu.area"

# Single-request headers (no Session, no HTTPAdapter)
HEADERS = {
    "User-Agent": "Asodya-Adapters/1.0 (+https://asodya.com) requests/py",
    "Accept": "text/plain,application/json;q=0.9,*/*;q=0.8",
}

class BlsgovAdapter(BaseAdapter):
    item_name = "blsgov"
    source_name = "public_and_gov"

    def get_preview(self) -> PreviewModel:
        return PreviewModel(
            mode=EnumMode.BOTH,
            source_name=self.source_name,
            has_topic=True,
            item_name=self.item_name,
            item_img="https://upload.wikimedia.org/wikipedia/commons/5/59/Seal_of_the_United_States_Bureau_of_Labor_Statistics.svg",
            updated_at=datetime.now(timezone.utc).isoformat(),
        )

    def get_topics(
        self,
        *,
        page: int = 1,
        per_page: int = 45,
        randomize: bool = False,
        seed: Optional[int] = None,
        **_: Any
    ) -> Dict[str, Any]:
        assert page >= 1 and per_page >= 1

        try:
            areas = self._fetch_selectable_areas()  # dynamic
        except Exception:
            # Never crash the API; return an empty page that still respects the contract.
            return {
                "topics": [],
                "page": page,
                "per_page": per_page,
                "has_more": False,
                "fetched_at": datetime.now(timezone.utc).isoformat(),
                "item_name": self.item_name,
                "source_name": self.source_name,
            }

        if randomize:
            rng = random.Random(seed) if seed is not None else random
            rng.shuffle(areas)

        start = (page - 1) * per_page
        end = start + per_page
        page_items = areas[start:end]

        topics = [
            {
                "id": self._to_series_id(area_code),                 # e.g., CUURS49ASA0
                "name": area_name,                                   # display city/metro
                "description": "United States",                      # BLS CPI scope
                "url": f"https://data.bls.gov/timeseries/{self._to_series_id(area_code)}",
            }
            for (area_code, area_name) in page_items
        ]

        return {
            "topics": topics,
            "page": page,
            "per_page": per_page,
            "has_more": end < len(areas),
            "fetched_at": datetime.now(timezone.utc).isoformat(),
            "item_name": self.item_name,
            "source_name": self.source_name,
        }

    # ---------- internals ----------
    def _fetch_selectable_areas(self) -> List[Tuple[str, str]]:
        """
        Downloads BLS CPI areas (cu.area) and returns (area_code, area_name) for selectable rows.
        No hard-coded area lists; whatever BLS marks selectable ('T') is included.
        """
        text = self._get_text_with_fallback()
        lines = text.splitlines()

        out: List[Tuple[str, str]] = []
        header_seen = False
        for row in lines:
            s = row.strip()
            if not s:
                continue
            if not header_seen:
                header_seen = True
                # first line is header (area_code area_name display_level selectable sort_sequence)
                if s.lower().startswith("area_code"):
                    continue  # skip header
                # If the file lacked a header, fall-through and parse.

            # Split by tabs if present; otherwise collapse multi-spaces
            cells = s.split("\t")
            if len(cells) < 2:
                cells = re.split(r"\s{2,}|\s+", s)

            # Expect at least 5 logical fields
            if len(cells) < 5:
                tokens = s.split()
                if len(tokens) < 5:
                    continue
                area_code = tokens[0]
                selectable = tokens[-2]
                area_name = " ".join(tokens[1:-3])
            else:
                area_code = cells[0].strip()
                selectable = cells[-2].strip()
                area_name = " ".join(c.strip() for c in cells[1:-3]).strip()

            if selectable.upper() != "T":
                continue
            if area_code and area_name:
                out.append((area_code, area_name))

        # Deterministic order when not randomizing
        out.sort(key=lambda t: t[1].lower())
        return out

    def _get_text_with_fallback(self) -> str:
        """
        Try HTTPS first; if we receive 403/401, retry via HTTP.
        Always pass a polite User-Agent. Raise for any 4xx/5xx.
        """
        https_url = f"https://{BLS_CU_AREA_PATH}"
        http_url  = f"http://{BLS_CU_AREA_PATH}"

        # 1) HTTPS attempt
        r = requests.get(https_url, headers=HEADERS, timeout=30)
        if r.status_code in (401, 403):
            # 2) Fallback to HTTP (BLS legacy host sometimes blocks HTTPS bots)
            r2 = requests.get(http_url, headers=HEADERS, timeout=30)
            r2.raise_for_status()
            return r2.text
        r.raise_for_status()
        return r.text

    @staticmethod
    def _to_series_id(area_code: str) -> str:
        # CPI-U, Not seasonally adjusted, "All items" = SA0
        return f"CUUR{area_code}SA0"
