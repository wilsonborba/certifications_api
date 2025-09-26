# src/dal/remote/devto_adapter.py
from __future__ import annotations

from typing import Any, Dict, List, Optional
from datetime import datetime, timezone
import email.utils as eut
import re
import requests
from bs4 import BeautifulSoup

from src.dal.remote.base import BaseAdapter
from src.domain.models.preview_model import PreviewModel, EnumMode
from src.domain.models.indentifications_model import IdentificationsModel

DEVTO_BASE = "https://dev.to"
DEVTO_TAGS = f"{DEVTO_BASE}/tags"         # ?page=N
DEVTO_FEED_GLOBAL = f"{DEVTO_BASE}/feed"  # (unused, but kept)
UA = "quiz-certify/1.0 (+https://asodya.com)"


class DevToAdapter(BaseAdapter):
    """
    Topics = Dev.to tags. Each topic's `input_identification` is the tag slug (e.g., 'react').
    Pagination: ?page=<n> on /tags.
    """
    item_name = "devto"
    source_name = "apps"

    # ---------- Preview ----------
    def get_preview(self) -> PreviewModel:
        return PreviewModel(
            mode=EnumMode.PLAYFUL,  # this source tilts playful; quizzes can still be factual
            source_name=self.source_name,
            has_topic=True,
            item_name=self.item_name,
            item_img="https://res.cloudinary.com/dhncdmb2t/image/upload/v1756446939/dev_to_vpmmrf.png",
            updated_at=datetime.now(timezone.utc).isoformat(),
        )

    # ---------- HTTP ----------
    def _get_html(self, url: str, params: Optional[Dict[str, Any]] = None) -> BeautifulSoup:
        r = requests.get(url, params=params or {}, timeout=20, headers={"User-Agent": UA})
        r.raise_for_status()
        return BeautifulSoup(r.text, "html.parser")

    def _get_xml(self, url: str, params: Optional[Dict[str, Any]] = None) -> BeautifulSoup:
        r = requests.get(url, params=params or {}, timeout=20, headers={"User-Agent": UA})
        r.raise_for_status()
        return BeautifulSoup(r.text, "xml")

    # ---------- Parsing helpers ----------
    def _parse_tag_cards(self, soup: BeautifulSoup) -> List[Dict[str, Any]]:
        topics: List[Dict[str, Any]] = []

        cards = soup.select("div.tag-card") or soup.select("[data-testid='tag-card']") or soup.select("li.tag-card")
        if not cards:
            seen = set()
            for a in soup.select("a[href^='/t/']"):
                href = a.get("href", "")
                if not href.startswith("/t/"):
                    continue
                slug = href.split("/t/", 1)[-1].strip("/").split("?", 1)[0]
                if not slug or slug in seen:
                    continue
                seen.add(slug)
                name = (a.get_text(strip=True) or slug).strip()
                desc_el = a.find_next("p")
                desc = (desc_el.get_text(" ", strip=True) if desc_el else "") or None
                topics.append({"type": "tag", "name": name, "slug": slug, "description": desc})
            return topics

        for card in cards:
            a = card.select_one("a[href^='/t/']") or card.find("a")
            if not a:
                continue
            href = a.get("href", "")
            if "/t/" not in href:
                continue
            slug = href.split("/t/", 1)[-1].strip("/").split("?", 1)[0]

            name_el = card.select_one("h3, h2, .crayons-tag__name") or a
            name = (name_el.get_text(" ", strip=True) if name_el else slug).strip()

            desc_el = card.select_one("p")
            desc = (desc_el.get_text(" ", strip=True) if desc_el else "") or None

            topics.append({"type": "tag", "name": name, "slug": slug, "description": desc})

        return topics

    # ---------- Public: Topics (tags only) ----------
    def get_topics(
        self,
        *,
        page: int = 1,
        per_page: int = 45,
        **_: Any
    ) -> Dict[str, Any]:
        assert page >= 1 and per_page >= 1

        soup = self._get_html(DEVTO_TAGS, params={"page": page})
        tags = self._parse_tag_cards(soup)

        topics = []
        for t in tags[:per_page]:
            slug = t["slug"]
            name = t["name"]
            tag_url = f"{DEVTO_BASE}/t/{slug}"

            topics.append({
                "type": "tag",
                "input_identification": slug,  # old stable ID
                "name": name,
                "description": t.get("description"),
                "url": tag_url,
                "identifications": IdentificationsModel(
                    input_identification=slug,
                    title_identification=name,
                    link_identification=tag_url,
                    img_link_identification=None,
                ),
            })

        has_more = bool(tags) and (len(tags) >= per_page)

        return {
            "topics": topics,
            "page": page,
            "per_page": per_page,
            "has_more": has_more,
            "updated_at": datetime.now(timezone.utc).isoformat(),
            "item_name": self.item_name,
            "source_name": self.source_name,
        }

    # ---------- Internals: tag context ----------
    def _fetch_tag_about(self, slug: str) -> Dict[str, Any]:
        url = f"{DEVTO_BASE}/t/{slug}"
        soup = self._get_html(url)

        desc = None
        cand = (
            soup.select_one(".tag-metadata p") or
            soup.select_one(".crayons-card p") or
            soup.select_one("header + p") or
            soup.find("p")
        )
        if cand:
            desc = cand.get_text(" ", strip=True) or None

        followers = None
        page_text = soup.get_text(" ", strip=True)
        m = re.search(r"([\d,]+)\s+followers", page_text, flags=re.I)
        if m:
            try:
                followers = int(m.group(1).replace(",", ""))
            except Exception:
                followers = None

        return {"url": url, "description": desc, "followers": followers}

    def _fetch_tag_feed(self, slug: str, max_items: int = 12) -> List[Dict[str, Any]]:
        feed_url = f"{DEVTO_BASE}/feed/tag/{slug}"
        soup = self._get_xml(feed_url)

        items = []
        for it in soup.select("item")[:max_items]:
            title = (it.title.get_text(strip=True) if it.title else None)
            link = (it.link.get_text(strip=True) if it.link else None)
            author = (it.find("dc:creator").get_text(strip=True) if it.find("dc:creator") else None)
            pub = (it.pubDate.get_text(strip=True) if it.pubDate else None)
            published_iso = None
            if pub:
                try:
                    dt = eut.parsedate_to_datetime(pub)
                    if dt.tzinfo is None:
                        dt = dt.replace(tzinfo=timezone.utc)
                    published_iso = dt.isoformat()
                except Exception:
                    published_iso = pub
            summary = None
            if it.description:
                summary = BeautifulSoup(it.description.get_text(), "html.parser").get_text(" ", strip=True)

            items.append({
                "title": title,
                "url": link,
                "author": author,
                "published": published_iso,
                "summary": summary,
            })

        return items

    # ---------- Input: full item for a tag ----------
    def get_input(
        self,
        *,
        input_identification: str | None = None,
        slug: str | None = None,
        max_items: int = 12,
        **_: Any
    ) -> Dict[str, Any]:
        now_iso = datetime.now(timezone.utc).isoformat()
        tag = (slug or input_identification or "").strip().lstrip("#").lower()
        if not tag:
            return {
                "input_identification": "",
                "input_data": {"error": "missing_tag_slug"},
                "updated_at": now_iso,
                "identifications": IdentificationsModel(
                    input_identification=None,
                    title_identification=None,
                    link_identification=None,
                    img_link_identification=None,
                ),
            }

        about = {}
        latest: List[Dict[str, Any]] = []
        try:
            about = self._fetch_tag_about(tag) or {}
        except Exception:
            about = {}

        try:
            latest = self._fetch_tag_feed(tag, max_items=max_items) or []
        except Exception:
            latest = []

        name = tag.replace("-", " ").title()

        input_data: Dict[str, Any] = {
            "meta": {
                "slug": tag,
                "name": name,
                "tag_url": f"{DEVTO_BASE}/t/{tag}",
            },
            "about": {
                "description": about.get("description"),
                "followers": about.get("followers"),
            },
            "latest": latest,
        }

        return {
            "input_identification": tag,
            "input_data": input_data,
            "updated_at": now_iso,
            "identifications": IdentificationsModel(
                input_identification=tag,
                title_identification=name,
                link_identification=f"{DEVTO_BASE}/t/{tag}",
                img_link_identification=None,
            ),
        }

    # ---------- Instructions ----------
    def instructions(self) -> str:
        return (
            "You will receive a Dev.to tag context: a short description, optional follower count, "
            "and a list of recent posts (title, author, date, summary). Create engaging quiz questions "
            "that can work in playful or serious modes.\n"
            "• Keep facts precise: names, dates, titles, and summaries must match the provided context.\n"
            "• Creativity welcome for phrasing and light humor; do not invent or speculate beyond the context.\n"
            "• External knowledge is fine only if it is common developer knowledge and clearly consistent with the items; "
            "never contradict the context.\n"
            "• Good question ideas: who-wrote-what, which post covers X, ordering by date, tag best practices mentioned, "
            "compare/contrast topics across two posts, ‘fill the blank’ using a title word, or short scenario questions "
            "grounded in the summaries.\n"
            "• You may receive content in a different language but always output in English."
            "• Avoid predictions, personal judgments, or sensitive topics. Keep wording clear, neutral, and concise."
        )

    # ---------- Search (tags + optional latest posts) ----------
    def search(
        self,
        q: str,
        *,
        page: int = 1,
        per_page: int = 30,
        mode: str = "substring",          # "fulltext" | "substring" | "fuzzy"
        max_tag_pages: int = 5,           # até quantas páginas de /tags varrer
        scan_posts: bool = True,          # se True, também procura nos posts do RSS da tag
        posts_per_tag: int = 8,           # quantos itens do RSS por tag carregar (para filtrar)
        **kwargs: Any,                    # <- exigência: absorve extras sem quebrar
    ) -> Dict[str, Any]:
        """
        Procura por tags (slug/nome/descrição) e opcionalmente por posts recentes
        de cada tag (título/autor/summary). Retorna o mesmo envelope de /topics.
        """
        assert isinstance(q, str) and q.strip(), "q deve ser não-vazio"
        assert page >= 1 and per_page >= 1
        qn = q.casefold()
        mode = mode if mode in ("fulltext", "substring", "fuzzy") else "substring"

        def _hit(text: Optional[str]) -> bool:
            t = (text or "").casefold()
            if not t:
                return False
            if mode in ("fulltext", "substring"):
                return qn in t
            # fuzzy
            return self._simple_fuzzy_score(t, qn) >= 0.78

        matched: List[Dict[str, Any]] = []

        # 1) varrer /tags paginado até max_tag_pages
        seen_slugs: set[str] = set()
        for p in range(1, max(1, int(max_tag_pages)) + 1):
            try:
                soup = self._get_html(DEVTO_TAGS, params={"page": p})
            except Exception:
                break
            tags = self._parse_tag_cards(soup)
            if not tags:
                break

            for t in tags:
                slug = t.get("slug") or ""
                name = t.get("name") or slug
                desc = t.get("description") or ""
                if not slug or slug in seen_slugs:
                    continue
                hay = " ".join([slug, name, desc])
                if not _hit(hay):
                    continue
                seen_slugs.add(slug)

                tag_url = f"{DEVTO_BASE}/t/{slug}"
                matched.append({
                    "type": "tag",
                    "name": name,
                    "description": desc or None,
                    "url": tag_url,
                    "identifications": IdentificationsModel(
                        input_identification=slug,
                        title_identification=name,
                        link_identification=tag_url,
                        img_link_identification=None,
                    ),
                })

                # 2) opcional: procurar nos posts do RSS dessa tag
                if scan_posts:
                    try:
                        feed_items = self._fetch_tag_feed(slug, max_items=posts_per_tag)
                    except Exception:
                        feed_items = []
                    for it in feed_items:
                        title = it.get("title") or ""
                        author = it.get("author") or ""
                        summary = it.get("summary") or ""
                        url = it.get("url") or ""
                        hay_post = " ".join([title, author, summary, url])
                        if not url or not _hit(hay_post):
                            continue
                        # para posts, usamos o link como ID estável o suficiente para o front
                        matched.append({
                            "type": "post",
                            "title": title,
                            "author": author or None,
                            "published": it.get("published"),
                            "url": url,
                            "summary": summary or None,
                            "tag": slug,
                            "identifications": IdentificationsModel(
                                input_identification=url,      # ID não-vazio p/ passar no shouldUseIdentifications
                                title_identification=title,
                                link_identification=url,
                                img_link_identification=None,
                            ),
                        })

            # Heurística: se já temos muitos matches, podemos parar cedo
            if len(matched) >= page * per_page * 2:
                break

        # 3) ordenação simples: tags primeiro por nome; depois posts por data (desc) e título
        def _key(item: Dict[str, Any]):
            t = item.get("type")
            if t == "tag":
                return (0, (item.get("name") or "").casefold(), "")
            # posts
            return (1, "", (item.get("published") or ""), (item.get("title") or "").casefold())

        matched.sort(key=_key, reverse=False)

        # 4) paginação sobre o resultado filtrado
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


    # ---------- Generate textual quiz context ----------
    def generate_context(self, input_data: Dict[str, Any], amount_question: int = 10) -> str:
        meta = (input_data or {}).get("meta", {})
        about = (input_data or {}).get("about", {})
        latest = (input_data or {}).get("latest", []) or []

        lines: List[str] = []
        lines.append("Dev.to Tag Context")
        lines.append(f"Tag: {meta.get('name') or meta.get('slug') or 'n/a'}  (/{meta.get('slug') or ''})")
        if meta.get("tag_url"): lines.append(f"Tag URL: {meta['tag_url']}")
        if about.get("description"): lines.append(f"Description: {about['description']}")
        if about.get("followers") is not None: lines.append(f"Followers: {about['followers']}")

        if latest:
            lines.append("\nRecent posts:")
            for it in latest:
                t = it.get("title") or "Untitled"
                a = it.get("author") or "Unknown"
                d = it.get("published") or "n/a"
                u = it.get("url") or ""
                s = it.get("summary") or ""
                lines.append(f"- {t} — by {a} on {d}")
                if s:
                    lines.append(f"  Summary: {s}")
                if u:
                    lines.append(f"  Link: {u}")

        lines.append("\nGuidance: Ask about facts shown here (authors, titles, dates, key ideas). "
                     "Be creative but keep details precise and grounded in this context.")

        context = "\n".join(lines)
        context += self.context_output_structure(amount_question=amount_question)
        return context
