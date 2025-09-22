# src/dal/remote/stackexchange_adapter.py

from __future__ import annotations
from typing import Any, Dict, Optional, List, Tuple
import re
import time
import requests
from datetime import datetime, timezone, timedelta

from src.domain.models.indentifications_model import IdentificationsModel
from src.core.settings import app_settings
from src.dal.remote.base import BaseAdapter
from src.domain.models.preview_model import PreviewModel, EnumMode

API_BASE = "https://api.stackexchange.com/2.3"

def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()

def _apply_backoff(payload: Dict[str, Any]) -> None:
    backoff = payload.get("backoff")
    if backoff:
        time.sleep(int(backoff))

# Built-in StackExchange filters:
# - 'withbody' includes post bodies (HTML).
# - For comments we can also request 'withbody'.
_QUESTION_FILTER = "withbody"
_ANSWER_FILTER = "withbody"
_COMMENT_FILTER = "withbody"

# Robust URL parsers for Stack Overflow style links
_QID_RE = re.compile(r"/questions/(\d+)(?:/|$)")
_AID_RE = re.compile(r"/a/(\d+)(?:/|$)|/answers/(\d+)(?:/|$)")

class StackExchangeOverflowAdapter(BaseAdapter):
    item_name = "stack_exchange_overflow"
    source_name = "apps"

    def __init__(self, site: str = "stackoverflow"):
        self.site = site
        self._key = app_settings().STACKEXCHANGEOVERFLOW_API_KEY

    def get_preview(self) -> PreviewModel:
        return PreviewModel(
            mode=EnumMode.BOTH,
            source_name=self.source_name,
            has_topic=True,
            item_name=self.item_name,
            item_img="https://res.cloudinary.com/dhncdmb2t/image/upload/v1756354735/Stack_Overflow_icon_ovjjbq.png",
            updated_at=_now_iso()
        )

    def instructions(self) -> str:
        """
        Guidance for question generation (parallel to RedditAdapter, but technical/factual).
        """
        return (
            "You are given a Stack Overflow question with answers and comments. "
            "Create clear, factual, and beginner-friendly quiz questions based strictly on the content. "
            "Focus on what the problem is, what solution worked, why it worked, relevant tags/technologies, "
            "and any code behaviors explicitly described. "
            "Avoid opinionated or controversial takes; stick to details present in the post/answers/comments. "
            "Use a concise, helpful tone. If useful, reference the question title or tags to ground the question. "
            "Write questions that most users can answer from the provided context alone."
        )

    # ------------ low-level HTTP ------------
    def _get(self, path: str, params: Dict[str, Any]) -> Dict[str, Any]:
        params = {**params, "site": self.site}
        if self._key:
            params["key"] = self._key
        r = requests.get(f"{API_BASE}{path}", params=params, timeout=15)
        r.raise_for_status()
        data = r.json()
        _apply_backoff(data)
        return data

    # ------------ helpers to resolve IDs from URLs ------------
    @staticmethod
    def _parse_ids_from_url(url: str) -> Tuple[Optional[int], Optional[int]]:
        """
        Returns (question_id, answer_id) if parseable, else (None, None).
        Supports:
          - https://stackoverflow.com/questions/<qid>/...
          - https://stackoverflow.com/a/<aid>
          - https://stackoverflow.com/questions/<qid>/.../<aid>#<aid>
          - https://stackoverflow.com/answers/<aid> (rare)
        """
        qid = None
        aid = None

        mq = _QID_RE.search(url)
        if mq:
            try:
                qid = int(mq.group(1))
            except Exception:
                qid = None

        ma = _AID_RE.search(url)
        if ma:
            g1, g2 = ma.group(1), ma.group(2)
            try:
                aid = int(g1 or g2)
            except Exception:
                aid = None

        return qid, aid

    # ------------ core fetchers ------------
    def _fetch_questions(
        self, *, page: int, pagesize: int, sort: str, tagged: Optional[str] = None, from_ts: Optional[int] = None
    ) -> Dict[str, Any]:
        params: Dict[str, Any] = {
            "page": page, "pagesize": pagesize, "order": "desc", "sort": sort
        }
        if tagged:
            params["tagged"] = tagged
        if from_ts is not None:
            params["fromdate"] = from_ts
        return self._get("/questions", params)

    def _fetch_answers_for_questions(self, qids: List[int]) -> Dict[int, Tuple[Optional[int], Optional[int]]]:
        if not qids:
            return {}
        ids_csv = ";".join(map(str, qids))
        data = self._get(f"/questions/{ids_csv}/answers", {"sort": "votes", "order": "desc"})
        by_q: Dict[int, List[Dict[str, Any]]] = {}
        for a in data.get("items", []):
            qid = a.get("question_id")
            if qid is None:
                continue
            by_q.setdefault(qid, []).append(a)

        qs = self._get(f"/questions/{ids_csv}", {"filter": "default"}).get("items", [])
        accepted_map = {q.get("question_id"): q.get("accepted_answer_id") for q in qs}

        res: Dict[int, Tuple[Optional[int], Optional[int]]] = {}
        for qid, answers in by_q.items():
            if not answers:
                res[qid] = (None, None)
                continue
            acc_id = accepted_map.get(qid)
            best_id = acc_id
            if best_id is None:
                best_id = max(answers, key=lambda a: a.get("score", 0)).get("answer_id")
            if len(answers) == 1:
                worst_id = answers[0].get("answer_id")
            else:
                worst_id = min(
                    [a for a in answers if a.get("answer_id") != best_id],
                    key=lambda a: a.get("score", 0),
                    default=answers[0]
                ).get("answer_id")
            res[qid] = (best_id, worst_id)
        return res

    # ------------ public: topics (already present) ------------
    def get_topics(
        self,
        *,
        page: int = 1,
        per_page: int = 30,
        tagged: Optional[str] = None,
        window_days: int = 7,
        **_: Any
    ) -> Dict[str, Any]:
        assert page >= 1 and per_page >= 1
        half = per_page // 2
        recent_quota = half + (per_page % 2)
        top_quota = half

        from_ts = int((datetime.now(timezone.utc) - timedelta(days=window_days)).timestamp())
        recent_raw = self._fetch_questions(page=page, pagesize=recent_quota, sort="creation", tagged=tagged, from_ts=from_ts)
        recent_items = [
            q for q in recent_raw.get("items", [])
            if (q.get("score", 0) >= 10 and q.get("answer_count", 0) >= 10)
        ]

        top_raw = self._fetch_questions(page=page, pagesize=top_quota, sort="votes", tagged=tagged)
        top_items = top_raw.get("items", [])

        combined = (recent_items + top_items)[:per_page]
        question_ids = [q.get("question_id") for q in combined if q.get("question_id")]
        qid_to_best_worst = self._fetch_answers_for_questions(question_ids) if question_ids else {}

        topics: List[Dict[str, Any]] = []
        for q in combined:
            qid = q.get("question_id")
            best_id, worst_id = qid_to_best_worst.get(qid, (None, None))

            t_topics = {
                "type": "question",

                "title": q.get("title"),
                "score": q.get("score"),
                "answer_count": q.get("answer_count"),
                "tags": q.get("tags"),
                "url": q.get("link"),
                "created": q.get("creation_date"),
                "owner": (q.get("owner") or {}).get("display_name"),
                "is_answered": q.get("is_answered"),
                "best_answer_id": best_id,
                "worst_answer_id": worst_id,
            }

            t_topics['identifications'] = IdentificationsModel(
                input_identification=f"q:{qid}" if qid else None,
                title_identification=q.get("title"),
                link_identification=q.get("link"),
            )

            topics.append(t_topics)

        has_more = bool(recent_raw.get("has_more")) or bool(top_raw.get("has_more"))

        return {
            "topics": topics,
            "page": page,
            "per_page": per_page,
            "has_more": has_more,
            "updated_at": _now_iso(),
            "item_name": self.item_name,
            "source_name": self.source_name,
        }

    # ------------ NEW: get_input (mirror of RedditAdapter) ------------
    def get_input(
        self,
        *,
        input_identification: str | None = None,   # preferred: question_id as string or "q:<id>", "a:<id>"
        topic_type: str | None = None,             # "question" | "answer"
        permalink_or_url: str | None = None,
        answers_limit: int = 10,
        comments_limit: int = 10,
        include_question_comments: bool = True,
        include_answer_comments: bool = False,
    ) -> Dict[str, Any]:
        """
        Fetch a full context for a Stack Overflow item.
        - If a URL is provided, tries to resolve question_id and/or answer_id.
        - If topic_type == 'answer' or an answer_id is present, fetches the parent question as well.
        Returns:
          {
            "input_identification": <canonical string>,
            "input_data": {
              "question": {...},
              "answers": [...],
              "comments": [...],          # question comments
              "answer_comments": {...}    # optional map: answer_id -> [comments]
            },
            "updated_at": <iso>,
          }
        """
        assert answers_limit >= 0 and comments_limit >= 0

        # Resolve IDs from URL if needed
        qid_from_url: Optional[int] = None
        aid_from_url: Optional[int] = None
        if not input_identification and permalink_or_url:
            qid_from_url, aid_from_url = self._parse_ids_from_url(permalink_or_url)

        # Normalize identification
        qid: Optional[int] = None
        aid: Optional[int] = None

        if input_identification:
            # Support "q:12345" / "a:67890" or plain "12345" (assume question)
            if input_identification.startswith("q:"):
                qid = int(input_identification[2:])
                topic_type = topic_type or "question"
            elif input_identification.startswith("a:"):
                aid = int(input_identification[2:])
                topic_type = "answer"
            else:
                # best-effort: assume numeric question id
                try:
                    qid = int(input_identification)
                    topic_type = topic_type or "question"
                except Exception:
                    pass

        if qid is None and qid_from_url is not None:
            qid = qid_from_url
        if aid is None and aid_from_url is not None:
            aid = aid_from_url
            topic_type = topic_type or "answer"

        if not qid and not aid:
            return {"error": "missing input_identification and permalink_or_url"}

        # If we only have an answer, fetch its parent question id
        if aid and not qid:
            ans = self._get(f"/answers/{aid}", {"filter": "default"}).get("items", [])
            if not ans:
                return {"error": "answer not found", "input_identification": f"a:{aid}"}
            qid = ans[0].get("question_id")

        if not qid:
            return {"error": "question not found", "input_identification": input_identification or permalink_or_url}

        # ---- Fetch the question (with body) ----
        q_items = self._get(f"/questions/{qid}", {"filter": _QUESTION_FILTER}).get("items", [])
        if not q_items:
            return {"error": "question not found", "input_identification": f"q:{qid}"}
        q = q_items[0]

        # ---- Fetch answers (with body), sorted by votes desc ----
        answers: List[Dict[str, Any]] = []
        if answers_limit > 0:
            a_items = self._get(
                f"/questions/{qid}/answers",
                {"sort": "votes", "order": "desc", "pagesize": min(answers_limit, 100), "filter": _ANSWER_FILTER}
            ).get("items", [])
            # If user gave a specific answer id, ensure it appears (even if beyond pagesize)
            if aid and all(a.get("answer_id") != aid for a in a_items):
                extra = self._get(f"/answers/{aid}", {"filter": _ANSWER_FILTER}).get("items", [])
                if extra:
                    a_items = [extra[0]] + a_items
            answers = a_items[:answers_limit]

        # ---- Fetch comments on the question (optional) ----
        q_comments: List[Dict[str, Any]] = []
        if include_question_comments and comments_limit > 0:
            q_comments = self._get(
                f"/questions/{qid}/comments",
                {"sort": "votes", "order": "desc", "pagesize": min(comments_limit, 100), "filter": _COMMENT_FILTER}
            ).get("items", [])

        # ---- Fetch comments for answers (optional) ----
        answer_comments_map: Dict[int, List[Dict[str, Any]]] = {}
        if include_answer_comments and comments_limit > 0 and answers:
            ids_csv = ";".join(str(a.get("answer_id")) for a in answers if a.get("answer_id"))
            if ids_csv:
                ac = self._get(
                    f"/answers/{ids_csv}/comments",
                    {"sort": "votes", "order": "desc", "pagesize": min(comments_limit, 100), "filter": _COMMENT_FILTER}
                ).get("items", [])
                for c in ac:
                    aid_for_c = c.get("post_id")
                    if aid_for_c:
                        answer_comments_map.setdefault(aid_for_c, []).append(c)

        # ---- Normalize payload ----
        def _owner_name(obj: Dict[str, Any]) -> Optional[str]:
            return (obj.get("owner") or {}).get("display_name")

        question_block = {
            "id": q.get("question_id"),
            "title": q.get("title"),
            "title_identification": q.get("title"),
            "body_html": q.get("body"),  # HTML string
            "tags": q.get("tags"),
            "owner": _owner_name(q),
            "link": q.get("link"),
            "score": q.get("score"),
            "answer_count": q.get("answer_count"),
            "is_answered": q.get("is_answered"),
            "accepted_answer_id": q.get("accepted_answer_id"),
            "creation_date": q.get("creation_date"),
        }

        answers_block = [{
            "id": a.get("answer_id"),
            "owner": _owner_name(a),
            "score": a.get("score"),
            "is_accepted": a.get("is_accepted"),
            "body_html": a.get("body"),
            "creation_date": a.get("creation_date"),
        } for a in answers]

        q_comments_block = [{
            "id": c.get("comment_id"),
            "owner": _owner_name(c),
            "score": c.get("score"),
            "body_html": c.get("body"),
            "creation_date": c.get("creation_date"),
        } for c in q_comments]

        # Canonical input_identification string
        canonical_id = f"q:{qid}" if not aid else f"q:{qid}|a:{aid}"

        input_ = {
            "input_data": {
                "question": question_block,
                "answers": answers_block,
                "comments": q_comments_block,
                "answer_comments": answer_comments_map if include_answer_comments else {},
            },
            "updated_at": _now_iso(),
        }

        input_['identifications'] = IdentificationsModel(
            input_identification=canonical_id,
            title_identification=q.get("title"),
            link_identification=q.get("link"),
            img_link_identification=None,
        )

        return input_
        

    # ------------ NEW: generate_context (mirror of RedditAdapter) ------------
    def generate_context(self, input_data: Dict[str, Any], amount_question: int = 10) -> str:
        """
        Build a readable context for quiz generation:
          - Question title, tags, high-level details
          - A compact version of the question body (HTML stripped lightly)
          - Top N answers (by score, already fetched as such)
          - Top comments (by score)
        """
        def _strip_html(s: Optional[str]) -> str:
            if not s:
                return ""
            # minimum, fast HTML stripper (we keep it lightweight; refine if needed)
            return re.sub(r"<[^>]+>", "", s).strip()

        q = input_data.get("question", {}) or {}
        answers = input_data.get("answers", []) or []
        comments = input_data.get("comments", []) or []

        title = q.get("title", "") or ""
        tags = q.get("tags", []) or []
        body = _strip_html(q.get("body_html")) or ""
        link = q.get("link", "") or ""
        accepted_id = q.get("accepted_answer_id")

        context = f"Stack Overflow question title: {title}\n"
        context += f"Tags: {', '.join(tags) if tags else '[none]'}\n"
        context += "Question body:\n"
        context += f"{body or '[no text]'}\n\n"

        if answers:
            context += "Top Answers (by votes):\n"
            for a in answers[:10]:
                ans_prefix = "[ACCEPTED] " if accepted_id and a.get("id") == accepted_id else ""
                owner = a.get("owner") or "unknown"
                score = a.get("score", 0)
                a_body = _strip_html(a.get("body_html"))[:1000]  # cap to keep prompt compact
                context += f"- {ans_prefix}{owner} ({score}): {a_body}\n"
            context += "\n"

        if comments:
            context += "Top Question Comments:\n"
            for c in comments[:10]:
                owner = c.get("owner") or "unknown"
                score = c.get("score", 0)
                c_body = _strip_html(c.get("body_html"))
                context += f"- {owner} ({score}): {c_body}\n"
            context += "\n"

        # Add the structure for the question generator
        context += self.context_output_structure(amount_question=amount_question)
        return context
