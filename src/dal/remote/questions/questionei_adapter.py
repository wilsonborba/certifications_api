import json
import re
from time import time
from urllib.parse import urlencode, urljoin, urlparse, parse_qs
from datetime import datetime, timezone
from typing import Any, Dict, List, Optional, Tuple

import requests
from bs4 import BeautifulSoup

from src.dal.remote.base import BaseAdapter
from src.domain.models.indentifications_model import IdentificationsModel
from src.domain.models.preview_model import EnumMode, PreviewModel  # pip install beautifulsoup4

BASE = "https://questionei.com"
HEADERS = {"User-Agent": "Asodya-Adapters/1.0 (+https://asodya.com)"}
QUESTION_HREF_RE = re.compile(r"^/questoes/(\d+)/?$", re.IGNORECASE)
SUBJECT_LIST_PATH = "/disciplinas/questoes/{slug}"

class QuestioneiAdapter(BaseAdapter):
    item_name = "questionei"
    source_name = "apps"

    SUBJECT_HREF_RE = re.compile(r"^/disciplinas/questoes/[^/?#]+", re.IGNORECASE)

    def get_preview(self) -> PreviewModel:
        return PreviewModel(
            mode=EnumMode.BOTH,
            source_name=self.source_name,
            has_topic=True,
            item_name=self.item_name,
            item_img="https://res.cloudinary.com/dhncdmb2t/image/upload/v1759674527/Screenshot_2025-10-05_212654_z36vec.png",
            updated_at=datetime.now(timezone.utc).isoformat(),
        )
    
    def instructions(self) -> str:
        """
        Use ONLY the provided Questionei content (statement, metadata, alternatives) to write
        multiple-choice questions. Requirements:

        • Output MUST be valid JSON in the exact structure provided by context_output_structure().
        • Each item must include: "question", "correct_answer", "options" (4 strings),
          "justification" (short, evidence-based), and "difficulty" (1–6, Bloom-like).
        • Prefer using the existing alternatives from the page. If some options are missing,
          craft plausible distractors aligned with the statement/meta.
        • Choose a single, best-supported correct_answer. If none is explicit, infer the
          most defensible option strictly from the text; do NOT leave placeholders like
          “not available” and do NOT omit the correct answer.
        • Keep wording precise and unambiguous; avoid external facts and speculation.
        • Justification must reference phrases/ideas that appear in the statement or meta
          (e.g., banca/ano/tema) and briefly explain why the correct option is right.
        """
        return (
            "Use ONLY the provided Questionei content (statement, metadata, alternatives) to write "
            "multiple-choice questions. Requirements:\n"
            "Choose a single, best-supported correct_answer. If none is explicit, infer the most defensible option strictly from the text;\n"
            "• Prefer page alternatives; if incomplete, create plausible distractors from the text/meta.\n"
            "• Pick one best-supported correct_answer—never use placeholders or omit it.\n"
            "• Be precise. Justify using phrases/ideas present in the statement/meta."
            "Justification must reference phrases/ideas that appear in the statement or meta (e.g., banca/ano/tema) and briefly explain why the correct option is right."
        )

    def get_topics(
        self,
        *,
        page: int = 1,
        per_page: int = 50,           # site shows ~10 per page; we’ll just return all we find on that page
        **_: Any,
    ) -> Dict[str, Any]:
        assert page >= 1 and per_page >= 1

        url = f"{BASE}/disciplinas?page={page}"
        try:
            html = self._get_html(url)
        except Exception:
            return self._empty(page, per_page)

        soup = BeautifulSoup(html, "html.parser")

        # ---- Extract subjects by stable href pattern ----
        subjects: List[Tuple[str, str]] = []  # (name, absolute_url)
        seen = set()
        for a in soup.find_all("a", href=True):
            href = a["href"].strip()
            if not self.SUBJECT_HREF_RE.match(href):
                continue
            # Ignore duplicates (pagination may repeat)
            abs_url = urljoin(BASE, href)
            # Text content is the subject name (ignore nested tags like <span> etc.)
            name = a.get_text(strip=True)
            if not name:
                continue
            key = (name, abs_url)
            if key in seen:
                continue
            seen.add(key)
            subjects.append((name, abs_url))

        # ---- Compute has_more robustly (no reliance on class names) ----
        has_more = self._has_more(soup, current_page=page)

        # Map to your Topic shape
        topics: List[Dict[str, Any]] = []
        for name, link in subjects:
            ident = self._slug(name)
            topics.append({
                "name": name,
                "description": None,
                "url": link,  # keep raw link for the client if you want
                "identifications": IdentificationsModel(
                    input_identification=ident,
                    title_identification=name,
                    link_identification=link,
                    img_link_identification=None,
                ),
            })

        # Optional: enforce per_page cap (even though site already paginates)
        topics = topics[:per_page]

        return {
            "topics": topics,
            "page": page,
            "per_page": per_page,
            "has_more": has_more,
            "updated_at": datetime.now(timezone.utc).isoformat(),
            "item_name": self.item_name,
            "source_name": self.source_name,
        }

    # ---------- helpers ----------

    def _get_html(self, url: str) -> str:
        delay = 0.4
        for _ in range(4):
            try:
                r = requests.get(url, headers=HEADERS, timeout=20)
                r.raise_for_status()
                return r.text
            except (requests.exceptions.RequestException, ValueError):
                time.sleep(delay)
                delay *= 2
        r = requests.get(url, headers=HEADERS, timeout=20)
        r.raise_for_status()
        return r.text

    def _has_more(self, soup: BeautifulSoup, *, current_page: int) -> bool:
        """
        Strategy 1: a <button> or <a> with name="next-page" and not disabled.
        Strategy 2 (fallback): look for page-number links and see if any > current_page.
        """
        # Strategy 1
        next_ctrl = soup.select_one('[name="next-page"]')
        if next_ctrl:
            disabled = (
                next_ctrl.has_attr("disabled")
                or "disabled" in (next_ctrl.get("class") or [])
                or next_ctrl.get("aria-disabled") in ("true", True)
            )
            if not disabled:
                return True

        # Strategy 2: parse any anchors with ?page=<n>
        max_seen = current_page
        for a in soup.find_all("a", href=True):
            href = a["href"]
            # only consider disciplina pagination links to reduce false positives
            if "/disciplinas" not in href:
                continue
            q = urlparse(urljoin(BASE, href)).query
            if not q:
                continue
            params = parse_qs(q)
            try:
                pvals = params.get("page") or []
                if not pvals:
                    continue
                n = int(pvals[0])
                if n > max_seen:
                    max_seen = n
            except (ValueError, TypeError):
                continue
        return max_seen > current_page

    def _slug(self, s: str) -> str:
        import unicodedata, re
        s = unicodedata.normalize("NFKD", s).encode("ascii", "ignore").decode("ascii")
        s = s.lower().strip()
        s = re.sub(r"[^a-z0-9]+", "-", s).strip("-")
        return s

    def _empty(self, page: int, per_page: int) -> Dict[str, Any]:
        return {
            "topics": [],
            "page": page,
            "per_page": per_page,
            "has_more": False,
            "updated_at": datetime.now(timezone.utc).isoformat(),
            "item_name": self.item_name,
            "source_name": self.source_name,
        }
    

    def get_input(
        self,
        *,
        input_identification: str | None = None,
        page: int = 1,
        per_page: int = 10,
        **_: Any,
    ) -> Dict[str, Any]:
        """
        Fetch the list of questions for a given subject (disciplina) and parse:
          - id, url, index on page
          - meta: year, banca, organizacao, disciplina, temas
          - text (full statement)
          - alternatives: [{letter, text}]
        This function is resilient to rotating classnames.

        Params:
          subject_slug: e.g. "administracao-de-recursos-materiais"
          page: list page number (1-based)
          per_page: ask the site to return up to this many (site cap is usually 10)

        Returns:
          {
            "identifications": IdentificationsModel(...),
            "input_data": {
               "subject": "...",
               "page": N,
               "per_page": M,
               "questions": [ ... ],
               "has_more": bool
            },
            "updated_at": "...iso..."
          }
        """
        now_iso = datetime.now(timezone.utc).isoformat()

        if not input_identification:
            return {
                "identifications": IdentificationsModel(
                    input_identification=None,
                    title_identification=None,
                    link_identification=None,
                    img_link_identification=None,
                ),
                "input_data": {},  # per your contract, empty on error
                "updated_at": now_iso,
            }

        # Build the list URL
        list_url = urljoin(
            BASE,
            SUBJECT_LIST_PATH.format(slug=input_identification)
        )
        query = {"page": page, "perPage": per_page}
        url = f"{list_url}?{urlencode(query)}"

        # Fetch page HTML
        try:
            html = self._get_html(url)
        except Exception:
            return {
                "identifications": IdentificationsModel(
                    input_identification=input_identification,
                    title_identification=input_identification.replace("-", " ").title(),
                    link_identification=url,
                    img_link_identification=None,
                ),
                "input_data": {},
                "updated_at": now_iso,
            }

        soup = BeautifulSoup(html, "html.parser")

        # Parse all question anchors, then expand to their container and fields
        questions: List[Dict[str, Any]] = []
        seen_ids = set()

        for a in soup.find_all("a", href=True):
            m = QUESTION_HREF_RE.match(a["href"].strip())
            if not m:
                continue
            qid = m.group(1)
            if qid in seen_ids:
                continue  # avoid duplicates if any
            seen_ids.add(qid)

            container = self._find_question_container(a)
            if not container:
                # fallback: at least capture id+url
                questions.append({
                    "id": qid,
                    "url": urljoin(BASE, a["href"].strip()),
                    "index": None,
                    "meta": {},
                    "text": None,
                    "alternatives": [],
                })
                continue

            # index (the number shown at the start of the card)
            index = self._extract_index_near(a)

            # meta block: spans with <strong>Label:</strong> value(+links)
            meta = self._extract_meta(container)

            # statement: prefer aria-label on the title/content block
            text = self._extract_statement(container)

            # alternatives: labels with radio inputs
            alternatives = self._extract_alternatives(container)
            if not any(a.get("text") for a in alternatives):
                # fallback: parse Next.js boot JSON from this same HTML
                alternatives = self._extract_alternatives_from_next_data(html)

            questions.append({
                "id": qid,
                "url": urljoin(BASE, a["href"].strip()),
                "index": index,
                "meta": meta,
                "text": text,
                "alternatives": alternatives,
            })

        # has_more: rely on pagination presence (same logic as topics)
        has_more = self._has_more(soup, current_page=page)

        ident_title = input_identification.replace("-", " ").title()

        return {
            "identifications": IdentificationsModel(
                input_identification=input_identification,
                title_identification=ident_title,
                link_identification=url,
                img_link_identification=None,
            ),
            "input_data": {
                "subject": input_identification,
                "page": page,
                "per_page": per_page,
                "questions": questions,
                "has_more": has_more,
            },
            "updated_at": now_iso,
        }

    # ---------- parsing helpers (classname-agnostic) ----------

    def _find_question_container(self, a_tag: "BeautifulSoup") -> Optional["BeautifulSoup"]:
        """
        Climb ancestors until we hit a <div> that also contains:
          - a descendant with an aria-label (the full text block), or
          - a 'Responder' button or a 'Gabarito comentado' footer item.
        """
        node = a_tag
        while node and node.name != "html":
            if node.name == "div":
                if node.find(attrs={"aria-label": True}):
                    return node
                # heuristic anchors
                if node.find("button", string=lambda s: s and "Responder" in s):
                    return node
                if node.find(string=lambda s: isinstance(s, str) and "Gabarito" in s):
                    return node
            node = node.parent
        return None

    def _extract_index_near(self, a_tag: "BeautifulSoup") -> Optional[int]:
        """
        The index number sits next to the id link within the header.
        We look at siblings inside the same header container.
        """
        header = a_tag.parent
        # climb once if it's not the <div> header
        if header and header.name != "div":
            header = header.parent
        if not header:
            return None
        # find a <p> containing just digits
        p = header.find("p")
        if p:
            txt = (p.get_text(strip=True) or "").strip()
            if txt.isdigit():
                try:
                    return int(txt)
                except ValueError:
                    return None
        # fallback: try any immediate text node with digits
        for sib in header.find_all(["p", "span"], recursive=False):
            txt = (sib.get_text(strip=True) or "")
            if txt.isdigit():
                return int(txt)
        return None

    def _extract_meta(self, container: "BeautifulSoup") -> Dict[str, Any]:
        """
        Meta is rendered as a sequence of <span><strong>Label:</strong> <a|text>...</span>
        We collect label -> value(s). Values can be text or [ {text, href}, ... ] if there are links.
        """
        meta: Dict[str, Any] = {}
        for span in container.find_all("span"):
            strong = span.find("strong")
            if not strong:
                continue
            label = strong.get_text(strip=True).rstrip(":")
            # Remove the <strong> part to get the value area
            # Clone span text then subtract label prefix
            raw = span.get_text(" ", strip=True)
            # If the span is "Label: value", remove the leading "Label:"
            value_text = raw[len(label) + 1 :].strip() if raw.startswith(label + ":") else raw

            links = [{"text": a.get_text(strip=True), "href": urljoin(BASE, a["href"].strip())}
                     for a in span.find_all("a", href=True)]
            if links:
                # If there are multiple links (e.g., Temas), keep the list
                meta[label] = links if len(links) > 1 else links[0]
            else:
                meta[label] = value_text or None
        return meta

    def _extract_statement(self, container: "BeautifulSoup") -> Optional[str]:
        """
        Prefer an element with aria-label (the site sets the whole question there).
        Fallback to the largest text block within the content area.
        """
        node = container.find(attrs={"aria-label": True})
        if node:
            # aria-label holds the entire statement without inner markup noise
            aria = node.get("aria-label")
            if isinstance(aria, str) and aria.strip():
                return " ".join(aria.split())
            # otherwise use text inside
            txt = node.get_text(" ", strip=True)
            if txt:
                return " ".join(txt.split())

        # Fallback: choose the longest <div> text inside this container
        best = ""
        for div in container.find_all("div"):
            txt = div.get_text(" ", strip=True)
            if txt and len(txt) > len(best):
                best = txt
        return best or None

    def _extract_alternatives(self, container: "BeautifulSoup") -> List[Dict[str, str]]:
        """
        Order of preference for each alternative's text:
        1) input[aria-label] if non-empty
        2) decode from input[id] pattern: "alternative-input-<LETTER>-<FULL TEXT>"
        3) <p> text (often empty server-side on this site)
        4) the label's own text without SVG noise

        Letter is taken from <span> when present; fallback to the LETTER in the id.
        """
        import re, html
        out: List[Dict[str, str]] = []

        def norm(s):
            if not s:
                return None
            return " ".join(str(s).split())

        # find all labels that contain a radio input
        for label in container.find_all("label"):
            inp = label.find("input", attrs={"type": "radio"})
            if not inp:
                continue

            # ---- letter
            letter = None
            sp = label.find("span")
            if sp:
                t = sp.get_text(strip=True)
                if t:
                    letter = t[:1]

            # ---- text
            text = norm(inp.get("aria-label"))

            if not text:
                # VERY IMPORTANT: decode from input id. Allow any unicode and don’t anchor to start.
                iid = inp.get("id") or ""
                # examples seen: "alternative-input-A-Produzir mercadorias ...", "alternative-input-B-Os códigos seguem ..."
                m = re.search(r"alternative-input-([A-Z])-(.+)$", iid, flags=re.DOTALL)
                if m:
                    if not letter:
                        letter = m.group(1)
                    decoded = html.unescape(m.group(2))
                    text = norm(decoded)

            if not text:
                # fallback to <p>
                p = label.find("p")
                if p:
                    text = norm(p.get_text(" ", strip=True))

            if not text:
                # last resort: strip SVGs then read the label text
                for svg in label.find_all("svg"):
                    svg.decompose()
                text = norm(label.get_text(" ", strip=True))

            if letter or text:
                out.append({"letter": letter, "text": text})

        return out

    def _extract_alternatives_from_next_data(self, html_text: str) -> List[Dict[str, str]]:
        """
        Parse the Next.js boot JSON (<script id="__NEXT_DATA__" type="application/json">)
        and heuristically locate alternatives for the current question.
        Returns [] if nothing useful is found.
        """
        import json

        # Grab the __NEXT_DATA__ payload
        start_marker = '<script id="__NEXT_DATA__" type="application/json">'
        end_marker = '</script>'
        i = html_text.find(start_marker)
        if i == -1:
            return []
        i += len(start_marker)
        j = html_text.find(end_marker, i)
        if j == -1:
            return []
        try:
            data = json.loads(html_text[i:j])
        except Exception:
            return []

        # Heuristic walk: look for any list under keys that look like alternatives/options
        # with short "letters" and non-empty "text" or plain strings.
        def walk(node):
            if isinstance(node, dict):
                for k, v in node.items():
                    # common key names we might see
                    if k.lower() in ("alternativas", "alternatives", "opcoes", "options", "respostas", "answers"):
                        yield v
                    yield from walk(v)
            elif isinstance(node, list):
                for it in node:
                    yield from walk(it)

        def normalize_list(lst):
            out = []
            if not isinstance(lst, list):
                return out
            for item in lst:
                if isinstance(item, dict):
                    # try {letter, text} shapes with many possible keyings
                    letter = (item.get("letter") or item.get("letra") or item.get("option") or item.get("id"))
                    text   = (item.get("text") or item.get("texto") or item.get("description") or item.get("value"))
                    if isinstance(letter, str) or isinstance(text, str):
                        out.append({"letter": (letter or None)[:1] if letter else None,
                                    "text":  " ".join(str(text or "").split()) or None})
                elif isinstance(item, str):
                    out.append({"letter": None, "text": " ".join(item.split())})
            return [x for x in out if x.get("text")]

        candidates = []
        for lst in walk(data):
            normalized = normalize_list(lst)
            if len(normalized) >= 2:
                candidates.append(normalized)

        # pick the biggest plausible candidate list
        if candidates:
            candidates.sort(key=len, reverse=True)
            return candidates[0]
        return []




    def search(
        self,
        *,
        q: str,
        page: int = 1,
        per_page: int = 30,
        **_: Any,
    ) -> Dict[str, Any]:
        """
        Search 'disciplinas' by text and return a topics page.
        Each topic = one subject (disciplina) with identifications and the subject URL.

        Input:
        q: user query (e.g., "administração")
        page, per_page: numeric pagination applied client-side to the found results

        Output (same envelope as get_topics):
        {
            "topics": [
            {
                "name": <subject name>,
                "description": "<N> questões encontradas" | None,
                "url": "https://questionei.com/disciplinas/questoes/<slug>?page=1&perPage=10",
                "identifications": IdentificationsModel(
                    input_identification="<slug>",
                    title_identification=<subject name>,
                    link_identification=<url>,
                    img_link_identification=None
                )
            }, ...
            ],
            "page": ...,
            "per_page": ...,
            "has_more": ...,
            "updated_at": <iso>,
            "item_name": "...",
            "source_name": "..."
        }
        """
        from urllib.parse import quote_plus, urljoin
        import re
        from datetime import datetime, timezone
        from bs4 import BeautifulSoup

        assert isinstance(q, str) and q.strip(), "search query 'q' must be a non-empty string"
        assert page >= 1 and per_page >= 1

        # 1) Fetch search page
        search_url = f"{BASE}/disciplinas?search={quote_plus(q.strip())}"
        try:
            html = self._get_html(search_url)
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

        soup = BeautifulSoup(html, "html.parser")

        # 2) Parse anchors that look like subject results
        # Example:
        # <a href="/disciplinas/questoes/administracao-de-recursos-materiais?page=1&perPage=10">
        #   <h2>Administração de Recursos Materiais</h2>
        #   <p><strong>8244</strong> questões encontradas</p>
        # </a>
        results = []
        seen_slugs = set()

        for a in soup.find_all("a", href=True):
            href = a["href"].strip()
            if not href.startswith("/disciplinas/questoes/"):
                continue

            # Extract slug
            m = re.search(r"/disciplinas/questoes/([^/?#]+)", href)
            if not m:
                continue
            slug = m.group(1)
            if slug in seen_slugs:
                continue
            seen_slugs.add(slug)

            # Subject name (prefer <h2>, else from slug)
            h2 = a.find("h2")
            name = h2.get_text(strip=True) if h2 else slug.replace("-", " ").title()

            # Question count (optional, from <strong>)
            strong = a.find("strong")
            count = None
            if strong:
                # Portuguese thousands often use '.' as separator
                raw = strong.get_text(strip=True)
                try:
                    count = int(raw.replace(".", "").replace(",", ""))
                except Exception:
                    count = None

            abs_url = urljoin(BASE, href)

            # Build one topic
            description = f"{count} questões encontradas" if isinstance(count, int) else None
            results.append({
                "name": name,
                "description": description,
                "url": abs_url,
                "identifications": IdentificationsModel(
                    input_identification=slug,
                    title_identification=name,
                    link_identification=abs_url,
                    img_link_identification=None,
                ),
            })

        # 3) Stable ordering (by name), then paginate locally
        results.sort(key=lambda t: (t.get("name") or "").casefold())

        start = (page - 1) * per_page
        end = start + per_page
        page_items = results[start:end]
        has_more = end < len(results)

        return {
            "topics": page_items,
            "page": page,
            "per_page": per_page,
            "has_more": has_more,
            "updated_at": datetime.now(timezone.utc).isoformat(),
            "item_name": self.item_name,
            "source_name": self.source_name,
        }

    

     # ---------------- context ----------------
    def generate_context(self, input_data: Dict[str, Any], amount_question: int = 10) -> str:
        """
        Builds a plain-text context string combining all key/value pairs in input_data
        and the model output structure, separated by newlines.
        """

        context_lines: list[str] = []

        # Safely iterate key/value pairs — stringify everything
        for key, value in (input_data or {}).items():
            # Represent complex values like dicts/lists in a readable way
            if isinstance(value, (dict, list, tuple, set)):
                context_lines.append(f"{key}: {json.dumps(value, ensure_ascii=False)}")
            else:
                context_lines.append(f"{key}: {value}")

        # Add your output structure
        output_structure = self.context_output_structure(amount_question=amount_question)
        context_lines.append(str(output_structure))

        # Join them all with newline separators
        return "\n".join(context_lines)