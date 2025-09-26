# src/dal/remote/killedbygoogle_adapter.py
from __future__ import annotations

import re
import html
from typing import Any, Dict, List, Optional
from datetime import datetime, timezone
from urllib.parse import urljoin, urlparse
import requests
from bs4 import BeautifulSoup

from src.domain.models.indentifications_model import IdentificationsModel
from src.dal.remote.base import BaseAdapter
from src.domain.models.preview_model import PreviewModel, EnumMode

BASE = "https://killedbygoogle.com"
UA = "quiz-certify/1.0 (+https://asodya.com)"


def _plain_text_from_html(html_str: str) -> str:
    """
    Strip HTML to compact readable text, removing scripts/styles/nav/footers,
    preferring <main> or <article> when present.
    """
    soup = BeautifulSoup(html_str or "", "html.parser")

    # remove noise
    for sel in ["script", "style", "noscript", "template", "svg"]:
        for t in soup.select(sel):
            t.decompose()
    for sel in ["nav", "footer", "header", "aside", ".sidebar", ".site-footer", ".site-header"]:
        for t in soup.select(sel):
            t.decompose()

    scope = soup.select_one("main") or soup.select_one("article") or soup.body or soup
    text = scope.get_text(" ", strip=True)
    return re.sub(r"\s+", " ", html.unescape(text)).strip()


def _slugify_name(name: str) -> str:
    s = (name or "").strip().lower()
    s = re.sub(r"[^a-z0-9]+", "-", s)
    return s.strip("-")


class KilledByGoogleAdapter(BaseAdapter):
    """
    Topics = discontinued Google products/services from killedbygoogle.com

    Contract (topics):
      {
        "topics": [{ "name": "<Product>", "url": "<canonical or external url>", "input_identification": "<slug>" }, ...],
        "page": int,
        "per_page": int,
        "has_more": bool,
        "updated_at": iso,
        "item_name": "killed_by_google",
        "source_name": "apps"
      }

    get_input(input_identification=<slug>) returns:
      {
        "input_identification": "<slug>",
        "input_data": {
          "meta": { name, slug, page_url, external_url, ... },
          "external_page_text": "<plain text excerpt ...>",
          # optional:
          # "site_page_text": "<plain text excerpt ...>",
          # "external_fetch_error": "external_fetch_failed"
        },
        "updated_at": iso
      }
    """
    item_name = "killed_by_google"
    source_name = "apps"

    # -------- Preview --------
    def get_preview(self) -> PreviewModel:
        return PreviewModel(
            mode=EnumMode.PLAYFUL,
            source_name=self.source_name,
            has_topic=True,
            item_name=self.item_name,
            item_img="https://res.cloudinary.com/dhncdmb2t/image/upload/v1756378449/Screenshot_2025-08-28_175351_stw8ji.png",
            updated_at=datetime.now(timezone.utc).isoformat(),
        )

    # -------- HTTP helpers --------
    def _get_json(self, path: str) -> Optional[List[Dict[str, Any]]]:
        """
        Back-compat helper that tries to return a list if possible.
        (Kept for callers already using it.)
        """
        try:
            r = requests.get(urljoin(BASE, path), timeout=20, headers={"User-Agent": UA})
            r.raise_for_status()
            # Don't rely on content-type strictly; parse JSON anyway.
            data = r.json()
            if isinstance(data, list):
                return data
            if isinstance(data, dict) and "graveyard" in data and isinstance(data["graveyard"], list):
                return data["graveyard"]
        except Exception:
            pass
        return None

    def _get_json_array(self, path: str) -> Optional[List[Dict[str, Any]]]:
        """
        Always try to return a list (or None). Normalizes multiple endpoints.
        """
        try:
            r = requests.get(urljoin(BASE, path), timeout=20, headers={"User-Agent": UA})
            r.raise_for_status()
            data = r.json()
            if isinstance(data, list):
                return data
            if isinstance(data, dict) and "graveyard" in data and isinstance(data["graveyard"], list):
                return data["graveyard"]
            return None
        except Exception:
            return None

    def _get_html(self, path: str = "/") -> Optional[BeautifulSoup]:
        try:
            r = requests.get(urljoin(BASE, path), timeout=20, headers={"User-Agent": UA})
            r.raise_for_status()
            return BeautifulSoup(r.text, "html.parser")
        except Exception:
            return None

    def _http_get_text(self, url: str) -> Optional[str]:
        try:
            r = requests.get(url, timeout=20, headers={"User-Agent": UA})
            r.raise_for_status()
            return r.text
        except Exception:
            return None

    # -------- JSON normalization --------
    def _topics_from_json(self, items: List[Dict[str, Any]]) -> List[Dict[str, str]]:
        topics: List[Dict[str, str]] = []
        for it in items:
            name = (it.get("name") or it.get("title") or it.get("product") or "").strip()
            if not name:
                continue

            slug = (it.get("slug") or "").strip().strip("/")
            if not slug:
                slug = _slugify_name(name)

            # Prefer explicit link; if site-relative, make absolute
            link = (it.get("link") or it.get("url") or it.get("source") or "").strip()
            if link and not link.startswith("http"):
                link = urljoin(BASE, link)

            # Fallback to canonical page per slug on killedbygoogle.com
            if not link:
                link = urljoin(BASE, f"/{slug}/")

            t_topics = {
                "name": name,
                "url": link,
                
            }

            t_topics['identifications'] = IdentificationsModel(
                input_identification=slug,
                title_identification=name,
                link_identification=link,
                img_link_identification=None,
            )
            

            topics.append(t_topics)
        return topics

    # -------- HTML fallback parsing --------
    def _topics_from_html(self, soup: BeautifulSoup) -> List[Dict[str, str]]:
        """
        Conservative fallback: extract <a> text as product name + href as URL,
        derive a slug from URL path or name.
        """
        topics: List[Dict[str, str]] = []
        for a in soup.select("a[href]"):
            text = (a.get_text(" ", strip=True) or "").strip()
            href = a.get("href") or ""
            if not text or len(text) < 2 or not href:
                continue

            # Ignore nav/footers or social links
            if any(x in href for x in ("/privacy", "/about", "twitter.com", "github.com", "mailto:")):
                continue

            url = href if href.startswith("http") else urljoin(BASE, href)

            # Try derive slug from path
            slug = ""
            try:
                path = urlparse(url).path.strip("/")
                slug = path.split("/", 1)[0] if path else ""
            except Exception:
                slug = ""

            if not slug:
                slug = _slugify_name(text)

            t_topics = {"name": text, "url": url}
            t_topics['identifications'] = IdentificationsModel(
                input_identification=slug,
                title_identification=text,
                link_identification=url,
                img_link_identification=None,
            )

            topics.append(t_topics)

        # Deduplicate by slug (keep first)
        seen = set()
        deduped: List[Dict[str, str]] = []
        for t in topics:
            s = t["identifications"].input_identification
            if s in seen:
                continue
            seen.add(s)
            deduped.append(t)
        return deduped

    # -------- Public: unified Topics --------
    def get_topics(
        self,
        *,
        page: int = 1,
        per_page: int = 30,
        **_: Any
    ) -> Dict[str, Any]:
        assert page >= 1 and per_page >= 1

        # Prefer JSON endpoints (the /api/graveyard is comprehensive)
        json_paths = ["/api/graveyard", "/api/killed.json", "/graveyard.json", "/graveyard.min.json"]
        items: Optional[List[Dict[str, Any]]] = None
        for p in json_paths:
            items = self._get_json_array(p)
            if items:
                break

        if items:
            all_topics = self._topics_from_json(items)
        else:
            # HTML fallback
            soup = self._get_html("/")
            all_topics = self._topics_from_html(soup) if soup else []

        # numeric paging (slice)
        start = (page - 1) * per_page
        end = start + per_page
        topics = all_topics[start:end]
        has_more = end < len(all_topics)

        return {
            "topics": topics,  # [{ "name": "...", "url": "...", "input_identification": "<slug>" }]
            "page": page,
            "per_page": per_page,
            "has_more": has_more,
            "updated_at": datetime.now(timezone.utc).isoformat(),
            "item_name": self.item_name,
            "source_name": self.source_name,
        }

    # -------- Input: fetch external context text --------
    def get_input(
        self,
        *,
        input_identification: str | None = None,
        max_external_chars: int = 25000,     # cap external page plain text
        include_site_page_text: bool = False, # set True to also fetch killedbygoogle product page text
        **_: Any
    ) -> Dict[str, Any]:
        now_iso = datetime.now(timezone.utc).isoformat()
        slug_or_name = (input_identification or "").strip().strip("/")

        # Load dataset (prefer the large API)
        items = (
            self._get_json_array("/api/graveyard")
            or self._get_json_array("/api/killed.json")
            or self._get_json_array("/graveyard.json")
            or self._get_json_array("/graveyard.min.json")
            or []
        )

        # Find record by slug first, then by exact name match fallback
        record: Optional[Dict[str, Any]] = None
        for it in items:
            s = (it.get("slug") or "").strip().strip("/")
            if s and s.lower() == slug_or_name.lower():
                record = it
                break
        if not record and slug_or_name:
            for it in items:
                nm = (it.get("name") or it.get("title") or it.get("product") or "").strip()
                if nm and nm.lower() == slug_or_name.lower():
                    record = it
                    break

        if not record:
            input_ =  {

                "input_data": {"error": "not_found"},
                "updated_at": now_iso,
            }

            input_['identifications'] = IdentificationsModel(
                input_identification=slug_or_name if slug_or_name else None,
                title_identification=None,
                link_identification=None,
                img_link_identification=None,
            )
            return input_

        # Normalize minimal meta
        name = (record.get("name") or record.get("title") or record.get("product") or slug_or_name).strip()
        slug = (record.get("slug") or _slugify_name(name)).strip().strip("/")
        external = (record.get("link") or record.get("url") or record.get("source") or "").strip()
        if external and not external.startswith("http"):
            external = urljoin(BASE, external)
        page_url = urljoin(BASE, f"/{slug}/")

        # Fetch external page and return plain text
        external_page_text = None
        external_fetch_error = None
        if external:
            html_text = self._http_get_text(external)
            if html_text:
                txt = _plain_text_from_html(html_text)
                if max_external_chars and len(txt) > max_external_chars:
                    txt = txt[:max_external_chars].rstrip() + " …"
                external_page_text = txt if txt else None
            else:
                external_fetch_error = "external_fetch_failed"

        # Optionally also fetch the site product page text
        site_page_text = None
        if include_site_page_text:
            site_html = self._http_get_text(page_url)
            if site_html:
                site_page_text = _plain_text_from_html(site_html)

        input_data: Dict[str, Any] = {
            "meta": {
                "name": name,
                "slug": slug,
                "page_url": page_url,
                "external_url": external or None,
            },
            "external_page_text": external_page_text,
        }
        if external_fetch_error:
            input_data["external_fetch_error"] = external_fetch_error
        if include_site_page_text and site_page_text:
            input_data["site_page_text"] = site_page_text

        input_2 = {
            
            "input_data": input_data,
            "updated_at": now_iso,
        }

        input_2['identifications'] = IdentificationsModel(
            input_identification=slug,
            title_identification=name,
            link_identification=page_url,
            img_link_identification=None,
        )

        return input_2

    # -------- Instructions & context-generation --------
    def instructions(self) -> str:
        return (
            "You’ll receive plain-text context about a discontinued Google product. "
            "Write engaging quiz questions for playful or serious modes. Be creative in phrasing, "
            "but keep facts (names, dates, reasons, timelines) exact and grounded in the provided text. "
            "Avoid speculation or private info."
        )
    

    def search(
        self,
        q: str,
        *,
        page: int = 1,
        per_page: int = 20,
        mode: str = "fulltext",          # "fulltext" | "substring" | "fuzzy"
        fill_page: bool = True,          # mantido por compatibilidade; aqui não é necessário
        max_extra_pages: int = 0,        # idem (dataset já vem inteiro)
        # adapter-specific (opcionais):
        search_in_description: bool = True,
        search_in_tags: bool = True,
        search_in_type: bool = True,
        **kwargs: Any,
    ) -> Dict[str, Any]:
        """
        Busca por produtos descontinuados em killedbygoogle:
        - Carrega o dataset JSON (graveyard).
        - Faz filtro local por substring ou fuzzy em:
          name | slug | (description/notes) | tipo | tags (quando existirem).
        - Pagina **o resultado filtrado** (page/per_page).
        - Retorna o mesmo envelope de /topics com 'identifications' (IdentificationsModel).
        """
        assert isinstance(q, str) and q.strip(), "q deve ser não-vazio"
        assert page >= 1 and per_page >= 1
        qn = q.casefold()
        mode = mode if mode in ("fulltext", "substring", "fuzzy") else "fulltext"

        # 1) Carregar dataset (preferindo o endpoint mais completo)
        items = (
            self._get_json_array("/api/graveyard")
            or self._get_json_array("/api/killed.json")
            or self._get_json_array("/graveyard.json")
            or self._get_json_array("/graveyard.min.json")
            or []
        )

        def _text_hay(it: Dict[str, Any]) -> str:
            name = (it.get("name") or it.get("title") or it.get("product") or "").strip()
            slug = (it.get("slug") or _slugify_name(name)).strip().strip("/")
            parts: List[str] = [name, slug]

            if search_in_type:
                parts.append((it.get("type") or "").strip())

            if search_in_tags:
                tags = it.get("tags") or it.get("categories") or []
                if isinstance(tags, list):
                    parts.extend([str(t) for t in tags])

            if search_in_description:
                for k in ("description", "notes", "reason", "blurb", "explanation"):
                    v = it.get(k)
                    if isinstance(v, str):
                        parts.append(v)

            hay = " ".join(p for p in parts if p)
            # normaliza HTML entities e espaços
            return html.unescape(re.sub(r"\s+", " ", hay)).casefold()

        def _match(it: Dict[str, Any]) -> bool:
            hay = _text_hay(it)
            if not hay:
                return False
            if mode in ("fulltext", "substring"):
                return qn in hay
            # fuzzy
            return self._simple_fuzzy_score(hay, qn) >= 0.78

        # 2) Filtrar e normalizar
        matched: List[Dict[str, Any]] = []
        for it in items:
            if not _match(it):
                continue

            name = (it.get("name") or it.get("title") or it.get("product") or "").strip()
            if not name:
                continue

            slug = (it.get("slug") or _slugify_name(name)).strip().strip("/")
            link = (it.get("link") or it.get("url") or it.get("source") or "").strip()
            if link and not link.startswith("http"):
                link = urljoin(BASE, link)
            if not link:
                link = urljoin(BASE, f"/{slug}/")

            topic = {
                "type": "product",
                "name": name,
                "url": link,
            }
            topic["identifications"] = IdentificationsModel(
                input_identification=slug,
                title_identification=name,
                link_identification=link,
                img_link_identification=None,
            )
            matched.append(topic)

        # 3) Ordenação previsível (alfabética por nome)
        matched.sort(key=lambda x: (x.get("name") or "").casefold())

        # 4) Paginação sobre o resultado filtrado
        start = (page - 1) * per_page
        end = start + per_page
        topics_page = matched[start:end]
        has_more = end < len(matched)

        return {
            "topics": topics_page,
            "page": page,
            "per_page": per_page,
            "has_more": has_more,
            "updated_at": datetime.now(timezone.utc).isoformat(),
            "item_name": self.item_name,
            "source_name": self.source_name,
        }

    def generate_context(self, input_data: Dict[str, Any], amount_question: int = 10) -> str:
        meta = (input_data or {}).get("meta", {})
        ext_txt = (input_data or {}).get("external_page_text")
        site_txt = (input_data or {}).get("site_page_text")

        lines: List[str] = []
        lines.append("Killed by Google – Product Context")
        if meta.get("name"):
            lines.append(f"Name: {meta['name']}")
        if meta.get("page_url"):
            lines.append(f"Site page: {meta['page_url']}")
        if meta.get("external_url"):
            lines.append(f"External source: {meta['external_url']}")

        if ext_txt:
            lines.append("")
            lines.append("External source text (plain):")
            lines.append(ext_txt)

        if site_txt:
            lines.append("")
            lines.append("Site page text (plain):")
            lines.append(site_txt)

        lines.append("")
        lines.append("Create questions grounded in this text (timelines, purpose, notable details).")

        context = "\n".join(lines)
        context += self.context_output_structure(amount_question=amount_question)
        return context
