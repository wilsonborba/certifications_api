# src/dal/remote/countriesnow_adapter.py
from __future__ import annotations
from typing import Any, Dict, List, Optional, Tuple
from datetime import datetime, timezone
import random, time, math
import requests

from src.dal.remote.base import BaseAdapter
from src.domain.models.preview_model import PreviewModel, EnumMode

BASE = "https://countriesnow.space/api/v0.1"
HEADERS = {"User-Agent": "Asodya-Adapters/1.0 (+https://asodya.com)"}

class CountriesnowAdapter(BaseAdapter):
    item_name = "countriesnow"
    source_name = "public_and_gov"

    # ---------- Preview ----------
    def get_preview(self) -> PreviewModel:
        return PreviewModel(
            mode=EnumMode.BOTH,
            source_name=self.source_name,
            has_topic=True,
            item_name=self.item_name,
            item_img="https://countriesnow.space/img/favicon.png",
            updated_at=datetime.now(timezone.utc).isoformat(),
        )

    # ---------- Topics (already provided) ----------
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

        def _slug(s: str) -> str:
            return s.lower().strip().replace(" ", "-")

        topics = [{
            # IMPORTANT: stable identification that get_input can reverse
            "input_identification": f"{_slug(city)}__{_slug(country)}",  # double-underscore to avoid ambiguity
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

    # ---------- NEW: Input ----------
    def get_input(
        self,
        *,
        input_identification: str | None = None,
        city: str | None = None,
        country: str | None = None,
        include_country_city_list_sample: int = 12,
        **_: Any
    ) -> Dict[str, Any]:
        """
        Resolve a single city/country and fetch concise, quiz-useful facts.

        Identification contract from get_topics:
          input_identification = "<city-slug>__<country-slug>"  (double underscore)

        Returns:
          {
            "input_identification": "...",
            "input_data": {
              "meta": {...},
              "country": {...},
              "city": {...}
            },
            "updated_at": "...iso..."
          }
        """
        now_iso = datetime.now(timezone.utc).isoformat()

        # ---- Resolve city/country names ----
        if not (city and country) and input_identification:
            # reverse the slug format produced in get_topics
            if "__" in input_identification:
                cslug, coslug = input_identification.split("__", 1)
                city = (cslug or "").replace("-", " ").strip()
                country = (coslug or "").replace("-", " ").strip()

        if not (city and country):
            return {
                "input_identification": input_identification or "",
                "input_data": {"error": "missing_city_or_country"},
                "updated_at": now_iso,
            }

        # Normalize letter case (API is case-insensitive but keep nice casing)
        city_title = city.title()
        country_title = country.title()

        # ---- Fetch country-level facts (best-effort; ignore failures) ----
        def _post_json(path: str, payload: dict) -> dict:
            try:
                r = requests.post(f"{BASE}{path}", headers=HEADERS, json=payload, timeout=20)
                r.raise_for_status()
                return r.json() or {}
            except Exception:
                return {}

        def _country_field(path: str, key: str) -> Any:
            data = _post_json(path, {"country": country_title})
            # most endpoints shape: {"error":false,"msg":"...","data":{...}}
            d = data.get("data")
            if isinstance(d, dict):
                return d.get(key)
            return None

        capital = _country_field("/countries/capital", "capital")
        currency = _country_field("/countries/currency", "currency")
        iso2 = _country_field("/countries/iso", "iso2")
        iso3 = _country_field("/countries/iso", "iso3")

        # flag (images endpoint returns url, svg, png, etc.)
        flag_data = _post_json("/countries/flag/images", {"country": country_title}).get("data") or {}
        flag_url = flag_data.get("flag") or flag_data.get("png") or flag_data.get("svg")

        # country city sample (helps comparisons)
        try:
            country_cities = self._get_cities_for_country(country_title)
        except Exception:
            country_cities = []
        if include_country_city_list_sample and country_cities:
            sample = country_cities[:include_country_city_list_sample]
        else:
            sample = []

        # Population by country (historical series, if the API supports it)
        # Endpoint commonly exists: POST /countries/population  {"country": "..."}
        pop_country = _post_json("/countries/population", {"country": country_title})
        pop_series = []
        latest_country_pop = None
        if isinstance(pop_country.get("data"), dict):
            # expected: {"country":"X","populationCounts":[{"year":2020,"value":...}, ...]}
            series = (pop_country["data"].get("populationCounts")) or []
            # normalize to simple list[{year, value}]
            for row in series:
                y = row.get("year")
                v = row.get("value")
                if isinstance(y, int) and isinstance(v, (int, float)):
                    pop_series.append({"year": y, "value": v})
            if pop_series:
                pop_series.sort(key=lambda r: r["year"])
                latest_country_pop = pop_series[-1]["value"]

        # City population (if available)
        # Endpoint often: POST /countries/population/cities {"city":"..."}
        pop_city_data = _post_json("/countries/population/cities", {"city": city_title})
        latest_city_pop = None
        city_pop_series = []
        if isinstance(pop_city_data.get("data"), list) and pop_city_data["data"]:
            # API may return multiple matches; pick first with counts
            entry = pop_city_data["data"][0]
            counts = entry.get("populationCounts") or []
            for row in counts:
                y = row.get("year")
                v = row.get("value")
                if isinstance(y, int) and isinstance(v, (int, float)):
                    city_pop_series.append({"year": y, "value": v})
            if city_pop_series:
                city_pop_series.sort(key=lambda r: r["year"])
                latest_city_pop = city_pop_series[-1]["value"]

        # Small helpers for deltas (%)
        def _pct(a: float | None, b: float | None) -> float | None:
            if a is None or b is None:
                return None
            if b == 0:
                return None
            return ((a - b) / b) * 100.0

        def _last_two_delta(series: List[Dict[str, Any]]) -> float | None:
            if len(series) < 2:
                return None
            a = series[-1]["value"]
            b = series[-2]["value"]
            return _pct(a, b)

        country_delta_pct = _last_two_delta(pop_series) if pop_series else None
        city_delta_pct = _last_two_delta(city_pop_series) if city_pop_series else None

        input_data: Dict[str, Any] = {
            "meta": {
                "city": city_title,
                "country": country_title,
                "iso2": iso2,
                "iso3": iso3,
                "capital": capital,
                "currency": currency,
                "flag_url": flag_url,
            },
            "country": {
                "latest_population": latest_country_pop,
                "population_series": pop_series[-12:] if pop_series else [],  # keep prompt lean
                "latest_population_change_pct": country_delta_pct,
                "sample_cities": sample,
            },
            "city": {
                "name": city_title,
                "latest_population": latest_city_pop,
                "population_series": city_pop_series[-12:] if city_pop_series else [],
                "latest_population_change_pct": city_delta_pct,
            },
        }

        # Rebuild the canonical identification (in case caller passed raw names)
        ident = f"{city_title.lower().replace(' ', '-')}" \
                f"__{country_title.lower().replace(' ', '-')}"
        return {
            "input_identification": ident,
            "input_data": input_data,
            "updated_at": now_iso,
        }

    # ---------- Instructions (concise, creativity-friendly) ----------
    def instructions(self) -> str:
        return (
            "You’ll receive a city + country bundle with precise facts (flag, capital, currency) "
            "and (when available) population series for the country and the city. "
            "Write engaging quiz questions usable in playful or serious modes.\n"
            "• Be exact with numbers, years, names—only use facts present in the context. "
            "You may add light flavor text, but never alter or guess facts.\n"
            "• External knowledge is okay only if it’s standard geography info and clearly consistent with the context; "
            "don’t contradict or speculate.\n"
            "• Good prompts: identify the capital/currency; compare latest vs. prior population; order cities by size "
            "(from the provided list); ‘which year had the highest value’; percent-change math; match flag to country.\n"
            "• Avoid politics, sensitive claims, or predictions. Keep wording clear, neutral, and concise."
        )

    # ---------- Generate textual quiz context ----------
    def generate_context(self, input_data: Dict[str, Any], amount_question: int = 10) -> str:
        """
        Produce a compact, human-readable context for question writing,
        then append the unified JSON-output instruction from BaseAdapter.
        """
        meta = (input_data or {}).get("meta", {})
        co = (input_data or {}).get("country", {})
        ci = (input_data or {}).get("city", {})

        lines: List[str] = []
        lines.append("Geography Context")
        lines.append(f"Country: {meta.get('country') or 'n/a'}  (ISO2: {meta.get('iso2') or 'n/a'}, ISO3: {meta.get('iso3') or 'n/a'})")
        if meta.get("capital"):  lines.append(f"Capital: {meta['capital']}")
        if meta.get("currency"): lines.append(f"Currency: {meta['currency']}")
        if meta.get("flag_url"): lines.append(f"Flag: {meta['flag_url']}")

        lines.append(f"Focus City: {ci.get('name') or 'n/a'}")
        if ci.get("latest_population") is not None:
            lines.append(f"City latest population: {ci['latest_population']:,}")
        if ci.get("latest_population_change_pct") is not None:
            lines.append(f"City latest change vs prior: {ci['latest_population_change_pct']:.2f}%")

        if co.get("latest_population") is not None:
            lines.append(f"Country latest population: {co['latest_population']:,}")
        if co.get("latest_population_change_pct") is not None:
            lines.append(f"Country latest change vs prior: {co['latest_population_change_pct']:.2f}%")

        # Short series tails for temporal questions
        city_series = ci.get("population_series") or []
        country_series = co.get("population_series") or []
        if city_series:
            lines.append("\nCity population series (year → value):")
            for row in city_series:
                lines.append(f"- {row['year']}: {row['value']:,}")
        if country_series:
            lines.append("\nCountry population series (year → value):")
            for row in country_series:
                lines.append(f"- {row['year']}: {row['value']:,}")

        sample = co.get("sample_cities") or []
        if sample:
            lines.append("\nOther cities in this country (sample):")
            lines.append(", ".join(sample))

        lines.append("\nGuidance: Ask about facts shown here (names, capital, currency, flag, years, values, % changes, ordering). "
                     "Be creative but keep facts precise and self-contained.")

        context = "\n".join(lines)
        context += self.context_output_structure(amount_question=amount_question)
        return context

    # ---------- internals ----------
    def _get_countries(self) -> List[Dict[str, str]]:
        url = f"{BASE}/countries"
        data = self._get_json(url)
        rows = data.get("data", [])
        out = [{"name": r.get("country")} for r in rows if r.get("country")]
        out.sort(key=lambda x: x["name"].lower())
        return out

    def _get_cities_for_country(self, country: str) -> List[str]:
        url = f"{BASE}/countries/cities"
        resp = requests.post(url, headers=HEADERS, json={"country": country}, timeout=20)
        resp.raise_for_status()
        data = resp.json()
        cities = data.get("data") or []
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
