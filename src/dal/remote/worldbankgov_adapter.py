from __future__ import annotations
from typing import Any, Dict, List, Optional
from datetime import datetime, timezone
import random
import requests

from src.dal.remote.base import BaseAdapter
from src.domain.models.preview_model import PreviewModel, EnumMode

WB_COUNTRIES_URL = "https://api.worldbank.org/v2/country"  # paginated; add ?format=json

class WorldbankgovAdapter(BaseAdapter):
    item_name = "worldbankgov"
    source_name = "public_and_gov"

    def get_preview(self) -> PreviewModel:
        return PreviewModel(
            mode=EnumMode.BOTH,
            source_name=self.source_name,
            has_topic=True,
            item_name=self.item_name,
            item_img="https://www.worldbank.org/content/dam/wbr/logo/wbg-logo.svg",
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

        # Pull ALL countries dynamically (handles WB pagination)
        countries = self._fetch_all_countries()

        # Optional: shuffle BEFORE slicing to support RandomCountry
        if randomize:
            rng = random.Random(seed) if seed is not None else random
            rng.shuffle(countries)

        start, end = (page - 1) * per_page, (page - 1) * per_page + per_page
        slice_ = countries[start:end]

        topics = [
            {
                # stable id = ISO3 code
                "id": c["id"],
                "name": c["name"],  # human display
                # put region or income level in description (compact context)
                "description": (c.get("region") or "") or None,
                "url": f"https://api.worldbank.org/v2/country/{c['iso2Code']}?format=json",
            }
            for c in slice_
        ]

        return {
            "topics": topics,
            "page": page,
            "per_page": per_page,
            "has_more": end < len(countries),
            "fetched_at": datetime.now(timezone.utc).isoformat(),
            "item_name": self.item_name,
            "source_name": self.source_name,
        }

    def _fetch_all_countries(self) -> List[Dict[str, Any]]:
        # World Bank is paginated; we iterate pages until we collect all countries.
        # We keep only real countries (region.id != "NA" which are aggregates).
        collected: List[Dict[str, Any]] = []
        page = 1
        while True:
            url = f"{WB_COUNTRIES_URL}?format=json&per_page=300&page={page}"
            r = requests.get(url, timeout=30)
            r.raise_for_status()
            data = r.json()
            if not isinstance(data, list) or len(data) < 2:
                break
            meta, rows = data[0], data[1]
            for row in rows:
                region = (row.get("region") or {}).get("value")
                if (row.get("region") or {}).get("id") == "NA":
                    continue  # skip aggregates per WB docs
                collected.append({
                    "id": row.get("id"),                # ISO3
                    "iso2Code": row.get("iso2Code"),
                    "name": row.get("name"),
                    "region": region,
                })
            # pagination end?
            total = int(meta.get("total", 0))
            per_page = int(meta.get("per_page", len(rows) or 1))
            if page * per_page >= total:
                break
            page += 1
        # deterministic order if not randomized
        collected.sort(key=lambda x: x["name"].lower())
        return collected
