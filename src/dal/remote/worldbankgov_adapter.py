from __future__ import annotations
from typing import Any, Dict, List, Optional, Tuple
from datetime import datetime, timezone
import random
import requests
import math

from src.dal.remote.base import BaseAdapter
from src.domain.models.preview_model import PreviewModel, EnumMode

WB_COUNTRIES_URL = "https://api.worldbank.org/v2/country"  # paginated; add ?format=json
WB_INDICATOR_URL = "https://api.worldbank.org/v2/country/{code}/indicator/{indicator}"  # ?format=json

def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()

# Default, broadly useful indicators (keep small to avoid heavy payloads)
_DEFAULT_INDICATORS: Dict[str, str] = {
    "SP.POP.TOTL": "Population, total",
    "NY.GDP.MKTP.CD": "GDP (current US$)",
    "NY.GDP.PCAP.CD": "GDP per capita (current US$)",
    "SP.DYN.LE00.IN": "Life expectancy at birth, total (years)",
    "EN.POP.DNST": "Population density (people per sq. km of land area)",
    "IT.NET.USER.ZS": "Individuals using the Internet (% of population)",
}

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
            updated_at=_now_iso(),
        )

    # ---------------- new: instructions ----------------
    def instructions(self) -> str:
        return (
            "You are given World Bank country facts and recent indicators. "
            "Create clear, factual, non-controversial quiz questions based strictly on the data. "
            "Focus on basics like population, GDP, GDP per capita, life expectancy, internet use, or density, "
            "as well as region or income group. Avoid subjective judgments. "
            "Keep questions short and answerable from the provided context."
        )

    # ---------------- topics (unchanged) ----------------
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
                "input_identification": c["id"],
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
            "updated_at": _now_iso(),
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
                    "incomeLevel": (row.get("incomeLevel") or {}).get("value"),
                    "capitalCity": row.get("capitalCity"),
                    "longitude": row.get("longitude"),
                    "latitude": row.get("latitude"),
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

    # ---------------- new: get_input ----------------
    def get_input(
        self,
        *,
        input_identification: str | None = None,   # ISO3 (preferred), ISO2, or country name
        country_code_or_name: str | None = None,
        indicators: Optional[Dict[str, str]] = None,  # {code: label}; defaults to _DEFAULT_INDICATORS
        years_back: int = 10,                         # how many recent years of series to include
        per_indicator_points: int = 8,                # cap points per indicator for payload size
    ) -> Dict[str, Any]:
        """
        Fetch country facts + a compact set of indicator series.
        - Accepts ISO3/ISO2/name via input_identification or country_code_or_name.
        - Returns:
          {
            "input_identification": "<ISO3>",
            "input_data": {
              "country": {...},
              "indicators": {
                 "<code>": {
                    "label": "...",
                    "latest": {"year": "YYYY", "value": <float or None>},
                    "series": [{"year":"YYYY","value":...}, ...]   # descending years, capped
                 },
                 ...
              }
            },
            "updated_at": "..."
          }
        """
        # ---- resolve which country ----
        ident = input_identification or country_code_or_name
        if not ident:
            return {"error": "missing input_identification and country_code_or_name"}

        # Pull all to match flexibly (ISO3, ISO2, or name)
        countries = self._fetch_all_countries()
        ident_norm = (ident or "").strip().lower()

        def _match(c: Dict[str, Any]) -> bool:
            return (
                (c.get("id") or "").lower() == ident_norm or
                (c.get("iso2Code") or "").lower() == ident_norm or
                (c.get("name") or "").strip().lower() == ident_norm
            )

        cands = [c for c in countries if _match(c)]
        if not cands:
            # try contains on name as a fallback (e.g., "Congo")
            cands = [c for c in countries if ident_norm in (c.get("name") or "").strip().lower()]
        if not cands:
            return {"error": "country not found", "input_identification": ident}

        country = cands[0]  # choose the first best match
        iso3 = country.get("id")
        iso2 = country.get("iso2Code")

        # ---- fetch richer country details from WB country endpoint (single) ----
        details = self._fetch_country_detail(iso2 or iso3)

        # ---- indicators (compact bundle) ----
        ind_map = indicators or _DEFAULT_INDICATORS
        ind_payload: Dict[str, Any] = {}
        for code, label in ind_map.items():
            series = self._fetch_indicator_series(iso3 or iso2, code, years_back=years_back, cap=per_indicator_points)
            latest = next((p for p in series if p.get("value") is not None), None)
            ind_payload[code] = {
                "label": label,
                "latest": latest,         # {"year": "YYYY", "value": ...} or None
                "series": series,         # descending by year, numeric values only where present
            }

        return {
            "input_identification": iso3 or iso2 or ident,
            "input_data": {
                "country": {
                    "iso3": iso3,
                    "iso2": iso2,
                    "name": country.get("name"),
                    "region": country.get("region"),
                    "incomeLevel": country.get("incomeLevel"),
                    "capitalCity": country.get("capitalCity"),
                    "latitude": country.get("latitude"),
                    "longitude": country.get("longitude"),
                    "detail": details,  # includes WB’s full country object for completeness
                },
                "indicators": ind_payload,
            },
            "updated_at": _now_iso(),
        }

    def _fetch_country_detail(self, code: str | None) -> Dict[str, Any]:
        if not code:
            return {}
        # WB country endpoint accepts ISO2 or ISO3
        url = f"{WB_COUNTRIES_URL}/{code}?format=json"
        r = requests.get(url, timeout=30)
        r.raise_for_status()
        data = r.json()
        if not isinstance(data, list) or len(data) < 2:
            return {}
        items = data[1] or []
        return items[0] if items else {}

    def _fetch_indicator_series(
        self,
        code: str | None,          # country ISO3 (preferred) or ISO2
        indicator: str,
        *,
        years_back: int = 10,
        cap: int = 8,
    ) -> List[Dict[str, Any]]:
        """
        Returns a descending (latest->older) list like:
        [{"year":"2023","value":123.0}, ...], capped to 'cap' points.
        """
        if not code:
            return []
        # Request plenty, then slice to the years we want
        url = WB_INDICATOR_URL.format(code=code, indicator=indicator) + "?format=json&per_page=200"
        r = requests.get(url, timeout=30)
        r.raise_for_status()
        data = r.json()
        if not isinstance(data, list) or len(data) < 2:
            return []

        rows = data[1] or []
        # Each row: {"date":"2022","value": <number or None>, ...}
        # Sort by year desc; keep most recent 'years_back' distinct years
        def _as_int_year(x: Dict[str, Any]) -> int:
            try:
                return int(x.get("date"))
            except Exception:
                return -10**9

        rows.sort(key=_as_int_year, reverse=True)

        # collect unique years up to years_back
        seen_years: set[str] = set()
        picked: List[Dict[str, Any]] = []
        for row in rows:
            y = str(row.get("date"))
            if y in seen_years:
                continue
            val = row.get("value")
            # normalize numeric (None is okay)
            if isinstance(val, (int, float)) or val is None:
                picked.append({"year": y, "value": (float(val) if isinstance(val, (int, float)) else None)})
                seen_years.add(y)
            if len(seen_years) >= years_back:
                break

        # final cap (payload control)
        return picked[:cap]

    # ---------------- new: generate_context ----------------
    def generate_context(self, input_data: Dict[str, Any], amount_question: int = 10) -> str:
        """
        Builds a compact, human-friendly context using:
        - Country basics (name, region, income, capital)
        - Latest snapshot for each indicator + small recent series (year:value)
        """
        c = (input_data or {}).get("country", {}) or {}
        inds: Dict[str, Any] = (input_data or {}).get("indicators", {}) or {}

        name = c.get("name") or "Unknown"
        region = c.get("region") or "Unknown"
        income = c.get("incomeLevel") or "Unknown"
        capital = c.get("capitalCity") or "Unknown"

        def _fmt_val(v: Optional[float]) -> str:
            if v is None or (isinstance(v, float) and (math.isnan(v) or math.isinf(v))):
                return "n/a"
            # Heuristic formatting: big numbers with separators, percents/years handled by label (kept simple)
            # Keep raw for GDP per capita/current USD but add grouping
            try:
                if abs(v) >= 1000:
                    return f"{v:,.0f}"
                # show one decimal if < 1000 but not an int
                if float(v).is_integer():
                    return f"{int(v)}"
                return f"{v:.1f}"
            except Exception:
                return str(v)

        context = []
        context.append(f"Country: {name}")
        context.append(f"Region: {region} | Income group: {income} | Capital: {capital}")
        context.append("")

        # Indicators
        if inds:
            context.append("Key indicators (latest available):")
            for code, bundle in inds.items():
                label = bundle.get("label") or code
                latest = bundle.get("latest") or {}
                ly, lv = latest.get("year"), latest.get("value")
                context.append(f"- {label}: { _fmt_val(lv) } (year {ly})")

            context.append("")
            context.append("Recent series (year → value):")
            # keep to a compact per-indicator listing
            for code, bundle in inds.items():
                label = bundle.get("label") or code
                series = bundle.get("series") or []
                if not series:
                    continue
                pairs = [f"{pt.get('year')}: {_fmt_val(pt.get('value'))}" for pt in series]
                context.append(f"- {label}: " + "; ".join(pairs))

        context.append("")
        # Append your standard output structure prompt
        context.append(self.context_output_structure(amount_question=amount_question))

        return "\n".join(context)
