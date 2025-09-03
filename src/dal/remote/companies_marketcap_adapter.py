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
        topics: List[Dict[str, str]] = []
        for td in soup.select("td.name-td"):
            name_el = td.select_one(".company-name")
            logo_el = td.select_one("img.company-logo")
            link_el = td.select_one(".name-div a[href]")

            if not name_el or not logo_el or not link_el:
                continue

            name = name_el.get_text(strip=True)
            logo_src = logo_el.get("src") or ""
            href = link_el.get("href") or ""  # e.g. /meta-platforms/marketcap/
            if not name or not logo_src or not href:
                continue

            logo_url = urljoin(BASE, logo_src)
            url = urljoin(BASE, href)

            # slug = part after leading slash, before next slash(es)
            # /meta-platforms/marketcap/ -> 'meta-platforms'
            slug = href.strip("/").split("/", 1)[0] if "/" in href.strip("/") else href.strip("/")

            topics.append({
                "type": "company",
                "topic_type": "company",
                "input_identification": slug,
                "name": name,
                "logo": logo_url,
                "url": url,
            })

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
            "updated_at": datetime.now(timezone.utc).isoformat(),
            "item_name": self.item_name,
            "source_name": self.source_name,
        }

    # ---------- Input: full company page ----------
    def get_input(
        self,
        *,
        input_identification: str | None = None,  # preferred: the slug (e.g., "meta-platforms")
        url: str | None = None,                   # alternatively: the full page URL
        **_: Any
    ) -> Dict[str, Any]:
        """
        Resolve and scrape a company's market cap page into quiz-friendly data.

        Returns:
        {
          "input_identification": "<slug-or-url>",
          "input_data": {...},
          "updated_at": "<iso>"
        }
        """
        ident = (input_identification or "").strip().strip("/")
        if url:
            page_url = url
        elif ident:
            # canonical marketcap page for a company slug
            page_url = f"{BASE}/{ident}/marketcap/"
        else:
            return {
                "input_identification": "",
                "input_data": {"error": "missing identification or url"},
                "updated_at": datetime.now(timezone.utc).isoformat(),
            }

        # Fetch & parse
        try:
            soup = self._get_html(page_url)
        except Exception as e:
            return {
                "input_identification": ident or page_url,
                "input_data": {"error": f"fetch_failed: {e}"},
                "updated_at": datetime.now(timezone.utc).isoformat(),
            }

        # Helpers
        def text_of(sel: str) -> Optional[str]:
            el = soup.select_one(sel)
            return el.get_text(strip=True) if el else None

        def all_info_boxes() -> Dict[str, str]:
            """
            Read all `.info-box` blocks and map their line2 labels -> line1 values.
            """
            out: Dict[str, str] = {}
            for box in soup.select(".info-box"):
                l1 = (box.select_one(".line1").get_text(" ", strip=True) if box.select_one(".line1") else "").strip()
                l2 = (box.select_one(".line2").get_text(" ", strip=True) if box.select_one(".line2") else "").strip()
                if l1 and l2:
                    out[l2] = l1
            return out

        def parse_money_to_number(s: str | None) -> Optional[float]:
            """
            Convert '$1.846 T' / '$909.62 B' to float USD.
            Returns None if parse fails. Keeps it best-effort.
            """
            if not s:
                return None
            t = s.replace(",", "").replace("$", "").strip()
            mult = 1.0
            if t.endswith(("T", "t")):
                mult = 1e12
                t = t[:-1]
            elif t.endswith(("B", "b")):
                mult = 1e9
                t = t[:-1]
            elif t.endswith(("M", "m")):
                mult = 1e6
                t = t[:-1]
            try:
                return float(t) * mult
            except Exception:
                return None

        # Top header: logo, name, ticker
        logo = None
        logo_el = soup.select_one("img.company-profile-logo")
        if logo_el and logo_el.get("src"):
            logo = urljoin(BASE, logo_el["src"])

        name = text_of(".company-title-container .company-name") or text_of(".long-company-name")
        ticker = text_of(".company-title-container .company-code")

        # Info boxes: Rank, Marketcap, Country, Share price, Change (1 day), Change (1 year)
        info = all_info_boxes()
        rank = info.get("Rank")
        marketcap_str = info.get("Marketcap")
        country = (soup.select_one('.info-box .line2:contains("Country")') or None)
        # Country often displayed with flag link in line1—capture from info map:
        country_str = info.get("Country")

        share_price = info.get("Share price")
        change_1d = info.get("Change (1 day)")
        change_1y = info.get("Change (1 year)")

        # Categories badges
        categories = [a.get_text(strip=True) for a in soup.select(".categories-box a.category-badge")]

        # Big H1/H2 around Market cap
        h1 = text_of(".profile-container h1")
        h2 = text_of(".profile-container h2 strong")

        # Narrative paragraph below H2 (often contains date and cap again)
        summary_para = text_of(".profile-container p.mt-2")

        # End-of-year table
        eoy_rows: List[Dict[str, Any]] = []
        for tr in soup.select('h3:contains("End of year Market Cap") ~ div table tbody tr'):
            tds = tr.select("td")
            if len(tds) >= 2:
                year = (tds[0].get_text(strip=True) if tds[0] else None)
                cap = (tds[1].get_text(strip=True) if tds[1] else None)
                change = (tds[2].get_text(strip=True) if len(tds) > 2 else None)
                eoy_rows.append({
                    "year": year,
                    "marketcap": cap,
                    "marketcap_usd": parse_money_to_number(cap),
                    "change": change,
                })

        # End-of-day sources
        eod_sources: List[Dict[str, Any]] = []
        for col in soup.select(".eod-marketcap-rows .eod-col"):
            cap = (col.select_one(".marketcap").get_text(strip=True) if col.select_one(".marketcap") else None)
            src_el = col.select_one("div a[href]")
            src_name = src_el.get_text(strip=True) if src_el else None
            src_url = urljoin(BASE, src_el.get("href")) if src_el and src_el.get("href") else None
            eod_sources.append({
                "reported_marketcap": cap,
                "reported_marketcap_usd": parse_money_to_number(cap),
                "source": src_name,
                "source_url": src_url,
            })

        input_data = {
            "type": "company",
            "slug": ident or None,
            "url": page_url,
            "name": name,
            "ticker": ticker,
            "logo": logo,
            "rank": rank,
            "marketcap": {
                "display": marketcap_str,
                "usd": parse_money_to_number(marketcap_str),
            },
            "share_price": share_price,
            "change_1d": change_1d,
            "change_1y": change_1y,
            "country": country_str,
            "categories": categories,
            "headline": h1,
            "headline_secondary": h2,
            "summary": summary_para,
            "end_of_year": eoy_rows,     # [{year, marketcap, marketcap_usd, change}, ...]
            "eod_sources": eod_sources,  # [{reported_marketcap, usd, source, source_url}, ...]
        }

        return {
            "input_identification": ident or page_url,
            "input_data": input_data,
            "updated_at": datetime.now(timezone.utc).isoformat(),
        }

    # ---------- Instructions (short & flexible) ----------
    def instructions(self) -> str:
        return (
            "You’ll receive market-cap pages for a public company (name, ticker, rank, current market cap, 1-day/1-year change, "
            "country, categories, and a table of end-of-year market caps). Write engaging quiz questions for playful or serious modes.\n"
            "• Be precise: numbers, ranks, dates, and comparisons must match the provided context.\n"
            "• Creativity welcome (fun phrasing, analogies), but never alter facts.\n"
            "• External facts are OK only if they are standard and clearly consistent; do not contradict or fill gaps with guesses.\n"
            "• Good angles: identify values, compare years, largest/smallest, percentage changes, ordering, categories, and country.\n"
            "• Avoid speculation (future prices), investment advice, or unverifiable claims. Keep wording clear and answerable from the context."
        )

    # ---------- Generate textual context ----------
    def generate_context(self, input_data: Dict[str, Any], amount_question: int = 10) -> str:
        d = input_data or {}
        lines: List[str] = []

        lines.append("CompaniesMarketCap Context")
        if d.get("name"):    lines.append(f"Company: {d['name']}")
        if d.get("ticker"):  lines.append(f"Ticker: {d['ticker']}")
        if d.get("url"):     lines.append(f"Source URL: {d['url']}")
        if d.get("rank"):    lines.append(f"Rank: {d['rank']}")
        mc = d.get("marketcap") or {}
        if mc.get("display"): lines.append(f"Current market cap: {mc['display']}")
        if d.get("share_price"): lines.append(f"Share price: {d['share_price']}")
        if d.get("change_1d"):   lines.append(f"Change (1 day): {d['change_1d']}")
        if d.get("change_1y"):   lines.append(f"Change (1 year): {d['change_1y']}")
        if d.get("country"):     lines.append(f"Country: {d['country']}")

        cats = d.get("categories") or []
        if cats:
            lines.append("Categories: " + ", ".join(cats))

        if d.get("headline"):           lines.append(f"Headline: {d['headline']}")
        if d.get("headline_secondary"): lines.append(f"Headline detail: {d['headline_secondary']}")
        if d.get("summary"):            lines.append(f"Summary: {d['summary']}")

        # End-of-year table (limit to keep prompt tight)
        eoy = d.get("end_of_year") or []
        if eoy:
            lines.append("")
            lines.append("End-of-year Market Cap (most recent first):")
            for row in eoy[:15]:
                y = row.get("year")
                cap = row.get("marketcap")
                chg = row.get("change")
                lines.append(f"- {y}: {cap}" + (f" ({chg})" if chg else ""))

        # EOD sources
        eod = d.get("eod_sources") or []
        if eod:
            lines.append("")
            lines.append("End-of-day market cap (by source):")
            for s in eod[:6]:
                v = s.get("reported_marketcap")
                src = s.get("source")
                lines.append(f"- {src or 'Source'}: {v}")

        lines.append("")
        lines.append("Guidance: Ask about exact values, rankings, year-over-year changes, ordering, categories, and country. "
                     "You may write playful or serious questions, but keep numbers precise and avoid speculation or advice.")

        context = "\n".join(lines)
        context += self.context_output_structure(amount_question=amount_question)
        return context

