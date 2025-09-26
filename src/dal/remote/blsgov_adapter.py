from __future__ import annotations
from typing import Any, Dict, List, Tuple, Optional
from datetime import datetime, timezone
import re
import random
import requests

from src.dal.remote.base import BaseAdapter
from src.domain.models.preview_model import PreviewModel, EnumMode
from src.domain.models.indentifications_model import IdentificationsModel  # <-- added

# BLS flat file (same path for http/https)
BLS_CU_AREA_PATH = "download.bls.gov/pub/time.series/cu/cu.area"
BLS_TS_API = "https://api.bls.gov/publicAPI/v2/timeseries/data/"

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
            item_img="https://res.cloudinary.com/dhncdmb2t/image/upload/v1757763640/Bureau_of_Labor_Statistics_logo_m1qamx.png",
            updated_at=datetime.now(timezone.utc).isoformat(),
        )

    def instructions(self) -> str:
        return (
            "You are given economic data from the U.S. Bureau of Labor Statistics (BLS). "
            "Your goal is to create clear and engaging quiz questions that can work in both SERIOUS and PLAYFUL modes. "
            "Always base your questions on precise numbers, dates, and trends from the data provided. "
            "\n\n"
            "Good directions:\n"
            "- Ask about index levels, increases/decreases, percent changes (MoM, YoY).\n"
            "- Highlight extremes (highest/lowest points), averages, streaks, or long-term changes.\n"
            "- Use comparisons if they can be directly supported by the data or closely related sources.\n"
            "- Keep tone neutral in SERIOUS mode; allow light humor or analogies in PLAYFUL mode.\n"
            "\n"
            "What to avoid:\n"
            "- Taking sides politically or socially.\n"
            "- Making claims or forecasts not supported by data.\n"
            "- Inventing values when data is missing.\n"
            "\n"
            "Extra guidance:\n"
            "- You may use related research or external knowledge, but only if it directly matches and supports the series data.\n"
            "- Questions must always be factually correct and precise. If unsure, skip the risky type.\n"
            "- Ensure no ambiguity: specify units, months, and years in stems and answers.\n"
            "- Distractors in multiple-choice should be plausible but wrong, not misleading.\n"
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

        topics = []
        for (area_code, area_name) in page_items:
            tsid = self._to_series_id(area_code)
            series_url = f"https://data.bls.gov/timeseries/{tsid}"
            topics.append({
                "name": area_name,                     # display city/metro
                "description": "United States",        # BLS CPI scope
                "url": series_url,
                # NEW: replace input_identification with identifications
                "identifications": IdentificationsModel(
                    input_identification=tsid,
                    title_identification=area_name,
                    link_identification=series_url,
                    img_link_identification=None,
                ),
            })

        return {
            "topics": topics,
            "page": page,
            "per_page": per_page,
            "has_more": end < len(areas),
            "updated_at": datetime.now(timezone.utc).isoformat(),
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
    
    def get_input(
        self,
        *,
        input_identification: str | None = None,
        start_year: int | None = None,
        end_year: int | None = None,
        recent: int | None = 24,            # default: last 24 points (good for quizzes)
        include_catalog: bool = True,
        registration_key: str | None = None,
        **_: Any
    ) -> Dict[str, Any]:
        """
        Return *quiz-ready* data for a CPI series.

        input_data schema:
        {
          "meta": {
            "series_id", "title", "area_code", "area_name",
            "periodicity", "seasonal", "units", "series_url"
          },
          "latest": {
            "date": "YYYY-MM", "year": 2025, "month": 8, "value": 307.11,
            "mom_change_pct": 0.23, "yoy_change_pct": 2.15
          },
          "window": [
            {"date":"2024-09","value":...}, ...   # oldest → newest
          ],
          "stats": {
            "count", "min_value","min_date","max_value","max_date",
            "mean","median","stdev",
            "net_change_pct_window",     # from first→last element of window
            "current_up_streak_months",  # consecutive months value↑ ending at latest
            "current_down_streak_months" # consecutive months value↓ ending at latest
          }
        }
        """
        tsid = (input_identification or "").strip()
        if not tsid:
            return {
                "identifications": IdentificationsModel(
                    input_identification=None,
                    title_identification=None,
                    link_identification=None,
                    img_link_identification=None,
                ),
                "input_data": {},
                "updated_at": datetime.now(timezone.utc).isoformat(),
            }

        # Build query params
        params: dict[str, Any] = {}
        if registration_key:
            params["registrationkey"] = registration_key

        if recent is not None and recent > 0:
            params["recent"] = int(recent)
        else:
            if start_year is not None:
                params["start_year"] = int(start_year)
            if end_year is not None:
                params["end_year"] = int(end_year)
            if start_year is None and end_year is None:
                params["latest"] = "true"

        if include_catalog:
            params["catalog"] = "true"

        # Fetch series
        url = f"{BLS_TS_API}{tsid}"
        try:
            r = requests.get(url, headers=HEADERS, params=params, timeout=30)
            r.raise_for_status()
            payload = r.json()
        except Exception as exc:
            return {
                "identifications": IdentificationsModel(
                    input_identification=tsid,
                    title_identification=None,
                    link_identification=f"https://data.bls.gov/timeseries/{tsid}",
                    img_link_identification=None,
                ),
                "input_data": {},
                "updated_at": datetime.now(timezone.utc).isoformat(),
            }

        results = (payload or {}).get("Results") or {}
        series_list = results.get("series") or []
        series_obj = series_list[0] if series_list else {}
        raw_points = series_obj.get("data") or []

        # Helpers
        def _month_int(period: str | None) -> int:
            if not period:
                return 0
            return int(period[1:]) if period.startswith("M") and period[1:].isdigit() else 0

        def _as_float(v) -> float | None:
            try:
                return float(v)
            except Exception:
                return None

        # Build chronological window (oldest→newest) with clean fields
        pts = []
        for dp in raw_points:
            y = int(dp.get("year", 0))
            m = _month_int(dp.get("period"))
            if y > 0 and 1 <= m <= 12:
                pts.append({
                    "year": y,
                    "month": m,
                    "value": _as_float(dp.get("value")),
                    "periodName": dp.get("periodName"),
                })
        pts.sort(key=lambda x: (x["year"], x["month"]))  # oldest→newest

        # Latest snapshot (use newest in chronological order)
        latest = pts[-1] if pts else None

        # Area name from cu.area (best-effort)
        area_name = None
        area_code = None
        if tsid.startswith("CUUR") and tsid.endswith("SA0") and len(tsid) > 7:
            area_code = tsid[4:-3]
            try:
                areas = dict(self._fetch_selectable_areas())
                area_name = areas.get(area_code)
            except Exception:
                pass

        # Catalog (title, seasonal, periodicity, units)
        title = None
        seasonal = None
        periodicity = None
        units = None
        cat_entry = None
        if include_catalog:
            for c in (results.get("catalog") or []):
                sid = c.get("series_id") or c.get("seriesID") or c.get("seriesid")
                if sid == tsid:
                    cat_entry = c
                    break
        if cat_entry:
            title = cat_entry.get("series_title") or cat_entry.get("seriesTitle")
            seasonal = cat_entry.get("seasonality") or cat_entry.get("seasonal")
            periodicity = cat_entry.get("periodicity")
            units = cat_entry.get("units")

        # Compute changes
        mom = None
        yoy = None
        if latest:
            # month-over-month vs previous element
            if len(pts) >= 2:
                v_now = latest["value"]
                v_prev = pts[-2]["value"]
                if v_now is not None and v_prev not in (None, 0):
                    mom = (v_now / v_prev - 1.0) * 100.0
            # year-over-year: find same month one year earlier
            y0, m0 = latest["year"], latest["month"]
            prev_same = next((p for p in pts if p["year"] == y0 - 1 and p["month"] == m0), None)
            if prev_same and latest["value"] is not None and prev_same["value"] not in (None, 0):
                yoy = (latest["value"] / prev_same["value"] - 1.0) * 100.0

        # Stats over window
        values = [p["value"] for p in pts if p["value"] is not None]
        count = len(values)
        min_value = min(values) if values else None
        max_value = max(values) if values else None
        min_date = None
        max_date = None
        if values:
            for p in pts:
                if p["value"] == min_value and min_date is None:
                    min_date = f'{p["year"]:04d}-{p["month"]:02d}'
                if p["value"] == max_value and max_date is None:
                    max_date = f'{p["year"]:04d}-{p["month"]:02d}'
        mean = (sum(values) / count) if count else None
        median = None
        stdev = None
        if count:
            sv = sorted(values)
            mid = count // 2
            if count % 2 == 1:
                median = sv[mid]
            else:
                median = (sv[mid - 1] + sv[mid]) / 2
            # sample stdev if count>1
            if count > 1:
                mu = mean
                var = sum((v - mu) ** 2 for v in values) / (count - 1)
                stdev = var ** 0.5

        net_change_pct_window = None
        if len(pts) >= 2 and pts[0]["value"] not in (None, 0) and pts[-1]["value"] is not None:
            net_change_pct_window = (pts[-1]["value"] / pts[0]["value"] - 1.0) * 100.0

        # Streaks ending at latest
        def _streak(direction: int) -> int:
            # direction: +1 for up, -1 for down; compares consecutive months ending at latest
            if len(pts) < 2:
                return 0
            streak = 0
            i = len(pts) - 1
            while i > 0:
                a = pts[i]["value"]
                b = pts[i - 1]["value"]
                if a is None or b is None:
                    break
                if direction == 1 and a > b:
                    streak += 1
                    i -= 1
                    continue
                if direction == -1 and a < b:
                    streak += 1
                    i -= 1
                    continue
                break
            return streak

        up_streak = _streak(+1)
        down_streak = _streak(-1)

        # Build output blocks
        meta = {
            "series_id": tsid,
            "title": title,
            "area_code": area_code,
            "area_name": area_name,
            "periodicity": periodicity,
            "seasonal": seasonal,
            "units": units,
            "series_url": f"https://data.bls.gov/timeseries/{tsid}",
        }

        latest_block = None
        if latest:
            latest_block = {
                "date": f'{latest["year"]:04d}-{latest["month"]:02d}',
                "year": latest["year"],
                "month": latest["month"],
                "value": latest["value"],
                "mom_change_pct": mom,
                "yoy_change_pct": yoy,
            }

        window = [
            {"date": f'{p["year"]:04d}-{p["month"]:02d}', "value": p["value"]}
            for p in pts
        ]

        stats = {
            "count": count,
            "min_value": min_value,
            "min_date": min_date,
            "max_value": max_value,
            "max_date": max_date,
            "mean": mean,
            "median": median,
            "stdev": stdev,
            "net_change_pct_window": net_change_pct_window,
            "current_up_streak_months": up_streak,
            "current_down_streak_months": down_streak,
        }

        input_data = {
            "meta": meta,
            "latest": latest_block,
            "window": window,
            "stats": stats,
        }

        # Choose a reasonable display title for identifications
        title_ident = title or (f"CPI — {area_name}" if area_name else tsid)

        return {
            "identifications": IdentificationsModel(
                input_identification=tsid,
                title_identification=title_ident,
                link_identification=meta["series_url"],
                img_link_identification=None,
            ),
            "input_data": input_data,
            "updated_at": datetime.now(timezone.utc).isoformat(),
        }
    
        # ---------- Search (areas + optional direct SeriesID) ----------
    def search(
        self,
        q: str,
        *,
        page: int = 1,
        per_page: int = 30,
        mode: str = "substring",          # "fulltext" | "substring" | "fuzzy"
        include_area_matches: bool = True,
        include_direct_series_id: bool = True,
        **kwargs: Any,                    # <- exigência: absorve extras sem quebrar
    ) -> Dict[str, Any]:
        """
        Procura por áreas do CPI (arquivo cu.area) e/ou aceita Series IDs diretos.
        Retorna o mesmo envelope de /topics (com `identifications`).

        - substring: case-insensitive em nome/código da área.
        - fuzzy: usa BaseAdapter._simple_fuzzy_score(...) em nome/código.
        - fulltext: alias de substring (não há endpoint full-text no BLS).
        """
        assert isinstance(q, str) and q.strip(), "q deve ser não-vazio"
        assert page >= 1 and per_page >= 1
        qn = q.strip().casefold()
        mode = mode if mode in ("fulltext", "substring", "fuzzy") else "substring"
        if mode == "fulltext":
            mode = "substring"

        def _hit(text: Optional[str]) -> bool:
            t = (text or "").casefold()
            if not t:
                return False
            if mode == "substring":
                return qn in t
            # fuzzy
            return self._simple_fuzzy_score(t, qn) >= 0.78

        topics: List[Dict[str, Any]] = []

        # 1) Match direto por Series ID (opcional)
        #    Aceita ex.: "CUUR0000SA0" ou "cuur0400sa0" etc.
        if include_direct_series_id:
            series_like = re.fullmatch(r"[A-Za-z]{2,5}[A-Za-z0-9]{5,}", q.strip())
            if series_like:
                tsid = q.strip().upper()
                area_name = None
                area_code = None
                if tsid.startswith("CUUR") and tsid.endswith("SA0") and len(tsid) > 7:
                    area_code = tsid[4:-3]
                    try:
                        areas = dict(self._fetch_selectable_areas())
                        area_name = areas.get(area_code)
                    except Exception:
                        pass
                series_url = f"https://data.bls.gov/timeseries/{tsid}"
                topics.append({
                    "type": "series",
                    "name": area_name or tsid,
                    "description": "BLS CPI series",
                    "url": series_url,
                    "identifications": IdentificationsModel(
                        input_identification=tsid,
                        title_identification=area_name or tsid,
                        link_identification=series_url,
                        img_link_identification=None,
                    ),
                })

        # 2) Procurar nas áreas (nome e código), gerando o Series ID canônico
        if include_area_matches:
            try:
                areas = self._fetch_selectable_areas()  # List[Tuple[area_code, area_name]]
            except Exception:
                areas = []

            for area_code, area_name in areas:
                hay = f"{area_code} {area_name}"
                if _hit(hay):
                    tsid = self._to_series_id(area_code)
                    series_url = f"https://data.bls.gov/timeseries/{tsid}"
                    topics.append({
                        "type": "area",
                        "name": area_name,
                        "description": "United States",
                        "url": series_url,
                        "identifications": IdentificationsModel(
                            input_identification=tsid,
                            title_identification=area_name,
                            link_identification=series_url,
                            img_link_identification=None,
                        ),
                    })

        # 3) Ordenar: áreas por nome; series_id diretos caem depois (estável)
        def _key(t: Dict[str, Any]):
            kind = t.get("type")
            if kind == "area":
                return (0, (t.get("name") or "").casefold())
            return (1, (t.get("name") or "").casefold())

        topics.sort(key=_key)

        # 4) Paginar
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


    def generate_context(self, input_data: Dict[str, Any], amount_question: int = 10) -> str:
        """
        Build a clear textual context from BLS CPI series data so the AI can craft quiz questions.
        Expects `input_data` in the structure produced by get_input():
        {
            "meta": {...},
            "latest": {...},
            "window": [{"date": "YYYY-MM", "value": float}, ...],
            "stats": {...}
        }
        """
        meta = input_data.get("meta", {}) or {}
        latest = input_data.get("latest", {}) or {}
        window = input_data.get("window", []) or []
        stats = input_data.get("stats", {}) or {}

        # Meta fields (with sensible fallbacks)
        series_id   = meta.get("series_id") or ""
        title       = meta.get("title") or "Consumer Price Index (All items, CPI-U)"
        area_code   = meta.get("area_code") or ""
        area_name   = meta.get("area_name") or ""
        periodicity = meta.get("periodicity") or "Monthly"
        seasonal    = meta.get("seasonal") or "Not seasonally adjusted"
        units       = meta.get("units") or "Index (1982-84=100)"
        series_url  = meta.get("series_url") or ""

        # Latest snapshot
        latest_date = latest.get("date") or "n/a"
        latest_year = latest.get("year")
        latest_month = latest.get("month")
        latest_value = latest.get("value")
        mom = latest.get("mom_change_pct")
        yoy = latest.get("yoy_change_pct")

        # Stats
        count = stats.get("count")
        min_value = stats.get("min_value")
        min_date = stats.get("min_date")
        max_value = stats.get("max_value")
        max_date = stats.get("max_date")
        mean = stats.get("mean")
        median = stats.get("median")
        stdev = stats.get("stdev")
        net_change_window = stats.get("net_change_pct_window")
        up_streak = stats.get("current_up_streak_months")
        down_streak = stats.get("current_down_streak_months")

        def fmt_pct(v):
            return f"{v:.2f}%" if isinstance(v, (int, float)) else "n/a"

        def fmt_num(v):
            return f"{v:,.3f}" if isinstance(v, (int, float)) else "n/a"

        # Header/context block
        context_lines: list[str] = []
        context_lines.append(f"BLS economic series overview")
        context_lines.append(f"Series ID: {series_id}")
        context_lines.append(f"Title: {title}")
        if area_name or area_code:
            context_lines.append(f"Geography: {area_name or 'n/a'}" + (f" (code {area_code})" if area_code else ""))
        context_lines.append(f"Periodicity: {periodicity}")
        context_lines.append(f"Seasonal adjustment: {seasonal}")
        context_lines.append(f"Units: {units}")
        if series_url:
            context_lines.append(f"Series page: {series_url}")
        context_lines.append("")

        # Latest details
        context_lines.append("Latest reading")
        context_lines.append(f"- Period: {latest_date} ({latest_year}-{latest_month if latest_month is not None else '??'})")
        context_lines.append(f"- Level: {fmt_num(latest_value)}")
        context_lines.append(f"- MoM change: {fmt_pct(mom)}")
        context_lines.append(f"- YoY change: {fmt_pct(yoy)}")
        context_lines.append("")

        # Recent window (chronological as provided)
        if window:
            context_lines.append("Recent values (chronological)")
            for row in window:
                dt = row.get("date") or "YYYY-MM"
                val = row.get("value")
                context_lines.append(f"- {dt}: {fmt_num(val)}")
            context_lines.append("")

        # Summary statistics
        context_lines.append("Window summary")
        context_lines.append(f"- Observations: {count if isinstance(count, int) else 'n/a'}")
        context_lines.append(f"- Min: {fmt_num(min_value)} ({min_date or 'n/a'})")
        context_lines.append(f"- Max: {fmt_num(max_value)} ({max_date or 'n/a'})")
        context_lines.append(f"- Mean: {fmt_num(mean)} | Median: {fmt_num(median)} | Std dev: {fmt_num(stdev)}")
        context_lines.append(f"- Net change over window: {fmt_pct(net_change_window)}")
        context_lines.append(f"- Current streak: up {up_streak or 0} months, down {down_streak or 0} months")
        context_lines.append("")

        context = "\n".join(context_lines)

        # IMPORTANT: append structure instructions required by BaseAdapter
        context += self.context_output_structure(amount_question=amount_question)

        return context
