# src/dal/remote/countriesnow_adapter.py
from __future__ import annotations
from typing import Any, Dict, List, Optional, Tuple
from datetime import datetime, timezone
import random, time, math
import requests

from src.dal.remote.base import BaseAdapter
from src.domain.models.preview_model import PreviewModel, EnumMode
from src.domain.models.indentifications_model import IdentificationsModel  # <-- added
from urllib.parse import quote_plus

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
        cap = need_until + 1  # collect one extra to detect "has_more"

        flat: List[Tuple[str, str]] = []  # (city, country)
        for i in idxs:
            if len(flat) >= cap:
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
                if len(flat) >= cap:
                    break

        # Paginate the flattened list
        start = (page - 1) * per_page
        end = start + per_page
        slice_ = flat[start:end]

        topics: List[Dict[str, Any]] = []
        for (city, country) in slice_:
            ident = f"{self._slug(city)}__{self._slug(country)}"
            title = f"{city} — {country}"
            topics.append({
                "name": city,
                "description": country,
                "url": None,
                "identifications": IdentificationsModel(
                    input_identification=ident,
                    title_identification=title,
                    link_identification=self._wiki_search_url(city, country),  # Wikipedia search link
                    img_link_identification=self.resolve_flag_url(iso2=None, country=country),  # image preview comes in get_input
                ),
            })

        return {
            "topics": topics,
            "page": page,
            "per_page": per_page,
            "has_more": len(flat) > end,  # true iff we found the sentinel
            "updated_at": datetime.now(timezone.utc).isoformat(),
            "item_name": self.item_name,
            "source_name": self.source_name,
        }


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
        identifications.input_identification = "<city-slug>__<country-slug>"  (double underscore)

        Returns (success):
        {
            "identifications": IdentificationsModel(...),
            "input_data": { "meta": {...}, "country": {...}, "city": {...} },
            "updated_at": "...iso..."
        }

        Returns (error):
        {
            "identifications": IdentificationsModel(...),
            "input_data": {},   # <- EMPTY per requirement
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
            # ERROR SHAPE: input_data must be {}
            return {
                "identifications": IdentificationsModel(
                    input_identification=(input_identification or None),
                    title_identification=None,
                    link_identification=None,
                    img_link_identification=None,
                ),
                "input_data": {},  # <- empty on error
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
            d = data.get("data")
            if isinstance(d, dict):
                return d.get(key)
            return None

        capital = _country_field("/countries/capital", "capital")
        currency = _country_field("/countries/currency", "currency")
        iso2 = _country_field("/countries/iso", "iso2")
        iso3 = _country_field("/countries/iso", "iso3")

        # flag (images endpoint returns url, svg, png, etc.)
        flag_url = self.resolve_flag_url(iso2=iso2, country=country_title)

        # country city sample (helps comparisons)
        try:
            country_cities = self._get_cities_for_country(country_title)
        except Exception:
            country_cities = []
        if include_country_city_list_sample and country_cities:
            sample = country_cities[:include_country_city_list_sample]
        else:
            sample = []

        # Population by country
        pop_country = _post_json("/countries/population", {"country": country_title})
        pop_series = []
        latest_country_pop = None
        if isinstance(pop_country.get("data"), dict):
            series = (pop_country["data"].get("populationCounts")) or []
            for row in series:
                y = row.get("year")
                v = row.get("value")
                if isinstance(y, int) and isinstance(v, (int, float)):
                    pop_series.append({"year": y, "value": v})
            if pop_series:
                pop_series.sort(key=lambda r: r["year"])
                latest_country_pop = pop_series[-1]["value"]

        # City population (if available)
        pop_city_data = _post_json("/countries/population/cities", {"city": city_title})
        latest_city_pop = None
        city_pop_series = []
        if isinstance(pop_city_data.get("data"), list) and pop_city_data["data"]:
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

        # Canonical identification (slug form)
        ident = f"{city_title.lower().replace(' ', '-')}" \
                f"__{country_title.lower().replace(' ', '-')}"
        title_ident = f"{city_title} — {country_title}"

        return {
            "identifications": IdentificationsModel(
                input_identification=ident,
                title_identification=title_ident,
                link_identification=self._wiki_search_url(city_title, country_title),  # Wikipedia search link
                img_link_identification=flag_url,                                      # flag preview
            ),
            "input_data": input_data,
            "updated_at": now_iso,
        }
    
    _flag_cache: dict[str, str | None] = {}

    def resolve_flag_url(self, iso2: str | None, country: str | None = None, *, size: str = "w320") -> str | None:
        """
        Resolve a PNG flag URL for a country.
        1) Fast path (no network): FlagCDN with iso2.
        2) Fallback: RestCountries by country name (accent/alias tolerant) -> flags.png or synthesize FlagCDN via cca2.
        Returns None if not resolvable.
        """

        # --- 1) Fast path: iso2 present -> FlagCDN (no API call) ---
        if iso2:
            return f"https://flagcdn.com/{size}/{iso2.lower()}.png"

        # --- 2) Fallback by name with small cache & normalization ---
        if not country:
            return None

        # Cache key: normalized country string
        key = self._normalize_country_key(country)
        if key in self._flag_cache:
            return self._flag_cache[key]

        # Normalize tricky aliases (common mismatches)
        q = self._canonical_country_query(country)

        try:
            # Ask RestCountries; limit fields for speed
            r = requests.get(
                f"https://restcountries.com/v3.1/name/{q}",
                params={"fields": "cca2,flags,name", "fullText": "false"},
                timeout=4,
            )
            if not r.ok:
                # last-chance: try without our normalization
                r = requests.get(
                    f"https://restcountries.com/v3.1/name/{country}",
                    params={"fields": "cca2,flags,name", "fullText": "false"},
                    timeout=4,
                )

            if r.ok:
                data = r.json()
                if isinstance(data, list) and data:
                    # choose best candidate by fuzzy-ish normalized name equality
                    want = self._pick_best_country_match(country, data)
                    if want:
                        png = (want.get("flags") or {}).get("png")
                        if png:
                            self._flag_cache[key] = png
                            return png
                        cca2 = (want.get("cca2") or "").lower()
                        if cca2:
                            url = f"https://flagcdn.com/{size}/{cca2}.png"
                            self._flag_cache[key] = url
                            return url
        except Exception:
            pass

        # fallthrough
        self._flag_cache[key] = None
        return None

    # ---- helpers ----

    @staticmethod
    def _normalize_country_key(name: str) -> str:
        # accent-insensitive, punctuation-light key
        import unicodedata, re
        s = unicodedata.normalize("NFKD", name).encode("ascii", "ignore").decode("ascii")
        s = s.lower()
        s = re.sub(r"[^a-z0-9]+", " ", s).strip()
        return s

    def _canonical_country_query(self, name: str) -> str:
        """
        Map common aliases to the form RestCountries expects better.
        You can extend this dict over time as you see cases.
        """
        key = self._normalize_country_key(name)
        aliases = {
            "cote d ivoire": "Côte d'Ivoire",
            "cotedivoire": "Côte d'Ivoire",
            "south korea": "Korea (Republic of)",
            "north korea": "Korea (Democratic People's Republic of)",
            "united states": "United States of America",
            "usa": "United States of America",
            "uk": "United Kingdom",
            "russia": "Russian Federation",
            "syria": "Syrian Arab Republic",
            "laos": "Lao People's Democratic Republic",
            "iraq": "Iraq",
            "iran": "Iran (Islamic Republic of)",
            "vietnam": "Viet Nam",
            "cape verde": "Cabo Verde",
            "swaziland": "Eswatini",
            "czech republic": "Czechia",
            "moldova": "Moldova (Republic of)",
            "tanzania": "Tanzania, United Republic of",
            "bolivia": "Bolivia (Plurinational State of)",
            "venezuela": "Venezuela (Bolivarian Republic of)",
            "brunei": "Brunei Darussalam",
            "macedonia": "North Macedonia",
            "palestine": "Palestine, State of",
            "micronesia": "Micronesia (Federated States of)",
            "congo": "Congo",
            "democratic republic of the congo": "Congo (Democratic Republic of the)",
            "republic of the congo": "Congo",
        }
        return aliases.get(key, name)

    def _pick_best_country_match(self, requested_name: str, candidates: list[dict]) -> dict | None:
        """
        Among RestCountries results, pick the one whose name is closest to the requested.
        We compare against multiple provided names (common/official).
        """
        req = self._normalize_country_key(requested_name)

        def keys_for(c: dict) -> list[str]:
            n = c.get("name") or {}
            corpus = []
            common = n.get("common")
            official = n.get("official")
            if common: corpus.append(common)
            if official: corpus.append(official)
            return [self._normalize_country_key(x) for x in corpus if isinstance(x, str)]

        # exact normalized match first
        for c in candidates:
            if req in keys_for(c):
                return c

        # otherwise use a simple length-min edit-distance heuristic
        # (avoids extra deps; good enough for our use)
        from difflib import SequenceMatcher
        best = None
        best_score = 0.0
        for c in candidates:
            for k in keys_for(c):
                score = SequenceMatcher(None, req, k).ratio()
                if score > best_score:
                    best_score = score
                    best = c
        return best
    
    def _slug(self, s: str) -> str:
        return s.lower().strip().replace(" ", "-")

    def _wiki_search_url(self, city: str, country: str) -> str:
        # Search is safer than trying to guess a canonical page path
        q = quote_plus(f"{city} {country}")
        return f"https://en.wikipedia.org/w/index.php?search={q}"

    def _maps_query_url(self, city: str, country: str) -> str:
        q = quote_plus(f"{city}, {country}")
        return f"https://www.google.com/maps/search/?api=1&query={q}"

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
    
        # ---------- Search (cities & countries) ----------
    def search(
        self,
        *args,
        q: str,
        page: int = 1,
        per_page: int = 30,
        mode: str = "substring",            # "substring" | "fuzzy" | "fulltext" (alias of substring)
        max_country_fetch: int = 12,        # safety cap: how many countries to expand into cities when scanning
        country_sample_if_country_only: int = 6,  # if query matches only a country, return up to N sample cities
        **kwargs: Any,
    ) -> Dict[str, Any]:
        """
        Finds city–country pairs matching `q`. We match against both city and country names.
        - substring: case-insensitive 'q in text'
        - fuzzy: BaseAdapter._simple_fuzzy_score(text, q) >= 0.78
        - fulltext: treated as 'substring' (remote API has no full-text search)

        Returns the same envelope as get_topics(), each item with IdentificationsModel.
        """
        assert isinstance(q, str) and q.strip(), "q must be a non-empty string"
        assert page >= 1 and per_page >= 1

        qn = q.strip().casefold()
        if mode not in ("substring", "fuzzy", "fulltext"):
            mode = "substring"
        if mode == "fulltext":
            mode = "substring"

        def _hit(text: Optional[str]) -> bool:
            t = (text or "").casefold()
            if not t:
                return False
            if mode == "substring":
                return qn in t
            return self._simple_fuzzy_score(t, qn) >= 0.78

        # Load countries once
        try:
            countries = self._get_countries()  # [{'name': '...'}, ...] sorted
        except Exception:
            return {
                "topics": [],
                "page": page,
                "per_page": per_page,
                "has_more": False,
                "updated_at": datetime.now(timezone.utc).isoformat(),
                "item_name": self.item_name,
                "source_name": self.source_name,
            }

        # 1) Prioritize countries whose name matches q; we will fetch their cities fully.
        matched_countries: List[str] = [c["name"] for c in countries if _hit(c["name"])]

        # 2) If no country hit, we need to scan countries and pull cities,
        #    but we cap expansions for performance.
        topics: List[Dict[str, Any]] = []
        fetched_countries = 0

        def _emit(city: str, country: str) -> None:
            ident = f"{self._slug(city)}__{self._slug(country)}"
            title = f"{city} — {country}"
            topics.append({
                "name": city,
                "description": country,
                "url": None,
                "identifications": IdentificationsModel(
                    input_identification=ident,
                    title_identification=title,
                    link_identification=self._wiki_search_url(city, country),
                    img_link_identification=self.resolve_flag_url(iso2=None, country=country),
                ),
            })

        # Helper: expand one country -> cities that match
        def _expand_country(country_name: str, need_city_match: bool = True) -> None:
            nonlocal fetched_countries
            if fetched_countries >= max_country_fetch:
                return
            try:
                cities = self._get_cities_for_country(country_name)
            except Exception:
                return
            fetched_countries += 1

            hits = []
            if need_city_match:
                for city in cities:
                    if _hit(city) or _hit(f"{city} {country_name}"):
                        hits.append(city)
            else:
                hits = cities[:country_sample_if_country_only] if country_sample_if_country_only > 0 else []

            for city in hits:
                _emit(city, country_name)

        # (A) Expand matched countries first (any city, but only emit those where city matches too)
        for cname in matched_countries:
            _expand_country(cname, need_city_match=True)

        # (B) If we still have room and the query looks like a city (or no country matched),
        #     scan other countries until we collect enough hits.
        if len(topics) < per_page:
            for c in countries:
                cname = c["name"]
                if cname in matched_countries:
                    continue
                if fetched_countries >= max_country_fetch:
                    break
                # pull cities and look for city matches
                before = len(topics)
                _expand_country(cname, need_city_match=True)
                if len(topics) >= per_page:
                    break
                # If nothing matched anywhere AND we have a direct country match scenario,
                # we will add a small sample later.

        # (C) If we matched only country names (no city hits) add a small sample per matched country
        if not topics and matched_countries:
            for cname in matched_countries:
                if fetched_countries >= max_country_fetch:
                    break
                _expand_country(cname, need_city_match=False)

        # Stable ordering: sort by (country, city)
        topics.sort(key=lambda t: ((t.get("description") or "").casefold(),
                                   (t.get("name") or "").casefold()))

        # Paginate
        start = (page - 1) * per_page
        end = start + per_page
        page_items = topics[start:end]
        has_more = end < len(topics)

        return {
            "topics": page_items,
            "page": page,
            "per_page": per_page,
            "has_more": has_more,
            "updated_at": datetime.now(timezone.utc).isoformat(),
            "item_name": self.item_name,
            "source_name": self.source_name,
        }


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
