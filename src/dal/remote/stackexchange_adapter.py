# src/dal/remote/stackexchange_adapter.py
from __future__ import annotations
from typing import Any, Dict, Optional, List, Tuple
import time
import requests
from datetime import datetime, timezone, timedelta

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

    # ------------ core fetchers ------------
    def _fetch_questions(
        self, *, page: int, pagesize: int, sort: str, tagged: Optional[str] = None, from_ts: Optional[int] = None
    ) -> Dict[str, Any]:
        params: Dict[str, Any] = {"page": page, "pagesize": pagesize, "order": "desc", "sort": sort}
        if tagged:
            params["tagged"] = tagged
        if from_ts is not None:
            params["fromdate"] = from_ts
        return self._get("/questions", params)

    def _fetch_answers_for_questions(self, qids: List[int]) -> Dict[int, Tuple[Optional[int], Optional[int]]]:
        """Return {question_id: (best_answer_id, worst_answer_id)} using votes (and accepted if present)."""
        if not qids:
            return {}
        ids_csv = ";".join(map(str, qids))
        # include body? not needed here; we only want ids & scores
        data = self._get(f"/questions/{ids_csv}/answers", {"sort": "votes", "order": "desc"})
        by_q: Dict[int, List[Dict[str, Any]]] = {}
        for a in data.get("items", []):
            qid = a.get("question_id")
            if qid is None:
                continue
            by_q.setdefault(qid, []).append(a)

        # Also need accepted IDs (live on the question); fetch minimal question fields
        qs = self._get(f"/questions/{ids_csv}", {"filter": "default"}).get("items", [])
        accepted_map = {q.get("question_id"): q.get("accepted_answer_id") for q in qs}

        res: Dict[int, Tuple[Optional[int], Optional[int]]] = {}
        for qid, answers in by_q.items():
            if not answers:
                res[qid] = (None, None)
                continue
            # best = accepted if present else highest score
            acc_id = accepted_map.get(qid)
            best_id = acc_id
            if best_id is None:
                best_id = max(answers, key=lambda a: a.get("score", 0)).get("answer_id")
            # worst = lowest score (not equal to best if >1)
            worst_id = None
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

    # ------------ unified: get_trends ------------
    def get_topics(
        self,
        *,
        page: int = 1,
        per_page: int = 30,
        tagged: Optional[str] = None,
        window_days: int = 7,     # recency window for the "recent+quality" half
        **_: Any
    ) -> Dict[str, Any]:
        """
        Mix 50/50:
          - half 'recent+quality' (creation-desc within window_days, score>=10 & answer_count>=10)
          - half 'top' (votes-desc, general high quality)
        Also returns IDs: question_id, best_answer_id, worst_answer_id (no bodies).
        """
        assert page >= 1 and per_page >= 1
        half = per_page // 2
        recent_quota = half + (per_page % 2)  # remainder goes to recent bucket
        top_quota = half

        # --- recent+quality bucket ---
        from_ts = int((datetime.now(timezone.utc) - timedelta(days=window_days)).timestamp())
        recent_raw = self._fetch_questions(page=page, pagesize=recent_quota, sort="creation", tagged=tagged, from_ts=from_ts)
        recent_items = [
            q for q in recent_raw.get("items", [])
            if (q.get("score", 0) >= 10 and q.get("answer_count", 0) >= 10)
        ]
        # If filter shrinks too much, we don't auto-walk more pages (keeps it simple/cheap).
        # You can later add a "top-up" loop if you want to guarantee full counts.

        # --- top bucket (votes desc) ---
        top_raw = self._fetch_questions(page=page, pagesize=top_quota, sort="votes", tagged=tagged)
        top_items = top_raw.get("items", [])

        # combine and normalize to unified 'trends'
        combined = (recent_items + top_items)[:per_page]
        question_ids = [q.get("question_id") for q in combined if q.get("question_id")]

        # optional: attach best/worst answer IDs
        qid_to_best_worst = self._fetch_answers_for_questions(question_ids) if question_ids else {}

        topics: List[Dict[str, Any]] = []
        for q in combined:
            qid = q.get("question_id")
            best_id, worst_id = qid_to_best_worst.get(qid, (None, None))
            topics.append({
                "type": "question",
                "question_id": qid,
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
            })

        # has_more if either query indicates more pages
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
