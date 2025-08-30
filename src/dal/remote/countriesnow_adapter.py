# src/dal/remote/countriesnow_adapter.py
from __future__ import annotations
from typing import Any, Dict, List, Optional, Tuple
from datetime import datetime, timezone
import random, time
import requests

from src.dal.remote.base import BaseAdapter
from src.domain.models.preview_model import PreviewModel, EnumMode

BASE = "https://countriesnow.space/api/v0.1"
HEADERS = {"User-Agent": "Asodya-Adapters/1.0 (+https://asodya.com)"}

class CountriesnowAdapter(BaseAdapter):
    
    item_name = "countriesnow"
    source_name = "public_and_gov"

    def get_preview(self) -> PreviewModel:
        return PreviewModel(
            mode=EnumMode.BOTH,
            source_name=self.source_name,
            has_topic=True,
            item_name=self.item_name,
            item_img="https://countriesnow.space/img/favicon.png",
            updated_at=datetime.now(timezone.utc).isoformat(),
        )

    def get_topics(
        self,
        *,
        page: int = 1,
        per_page: int = 45,
        randomize: bool = False,
        seed: Optional[int] = None,
        **_: Any,
    ) -> Dict[str, Any]:
        assert page >= 1 and per_page >= 1

        try:
            countries = self._get_countries()   # [{"name": "..."}...]
        except Exception:
            return self._empty(page, per_page)

        # Choose an ordering of countries (randomized or deterministic)
        idxs = list(range(len(countries)))
        rng = random.Random(seed) if (randomize and seed is not None) else (random if randomize else None)
        if rng:
            rng.shuffle(idxs)

        # Flatten cities from countries until we can serve the requested page slice
        need_until = page * per_page
        flat: List[Tuple[str, str]] = []  # (city, country)
        for i in idxs if rng else idxs:
            if len(flat) >= need_until:
                break
            cname = countries[i]["name"]
            try:
                cities = self._get_cities_for_country(cname)  # list[str]
            except Exception:
                continue
            # Optionally shuffle cities per country when randomize=True
            if rng:
                rng.shuffle(cities)
            for city in cities:
                flat.append((city, cname))
                if len(flat) >= need_until:
                    break

        # Paginate the flattened list
        start = (page - 1) * per_page
        end = start + per_page
        slice_ = flat[start:end]

        topics = [{
            "id": f"{city}-{country}".lower().replace(" ", "-"),
            "name": city,
            "description": country,
            "url": None
        } for (city, country) in slice_]

        return {
            "topics": topics,
            "page": page,
            "per_page": per_page,
            "has_more": len(flat) > end,
            "updated_at": datetime.now(timezone.utc).isoformat(),
            "item_name": self.item_name,
            "source_name": self.source_name,
        }

    # ---- internals ----
    def _get_countries(self) -> List[Dict[str, str]]:
        url = f"{BASE}/countries"
        data = self._get_json(url)
        # shape: {"error": false, "msg": "...", "data": [{"country":"Afghanistan","iso2":"AF","iso3":"AFG"}, ...]}
        rows = data.get("data", [])
        out = [{"name": r.get("country")} for r in rows if r.get("country")]
        # deterministic order if not randomized
        out.sort(key=lambda x: x["name"].lower())
        return out

    def _get_cities_for_country(self, country: str) -> List[str]:
        url = f"{BASE}/countries/cities"
        resp = requests.post(url, headers=HEADERS, json={"country": country}, timeout=20)
        resp.raise_for_status()
        data = resp.json()
        cities = data.get("data") or []
        # keep simple strings only
        return [c for c in cities if isinstance(c, str) and c.strip()]

    def _get_json(self, url: str) -> Dict[str, Any]:
        delay = 0.4
        for _ in range(4):
            try:
                resp = requests.get(url, headers=HEADERS, timeout=20)
                resp.raise_for_status()
                return resp.json()
            except (requests.exceptions.RequestException, ValueError):
                time.sleep(delay)
                delay *= 2
        # final raise to be caught by caller
        resp = requests.get(url, headers=HEADERS, timeout=20)
        resp.raise_for_status()
        return resp.json()

    def _empty(self, page: int, per_page: int) -> Dict[str, Any]:
        return {
            "topics": [],
            "page": page,
            "per_page": per_page,
            "has_more": False,
            "fetched_at": datetime.now(timezone.utc).isoformat(),
            "item_name": self.item_name,
            "source_name": self.source_name,
        }
