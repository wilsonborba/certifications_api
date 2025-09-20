

from typing import List
from abc import ABC, abstractmethod
import re, unicodedata, hashlib
from typing import List, Dict, Any, Optional
from math import sqrt
from difflib import SequenceMatcher
from src.core.logs import debug, error

def _cosine_sim(vec_a: list[float], vec_b: list[float]) -> float:
    # Guard

    debug(f"Calculating cosine similarity between vectors of lengths {len(vec_a)} and {len(vec_b)}")

    if not vec_a or not vec_b or len(vec_a) != len(vec_b):
        return 0.0
    dot = sum(a*b for a, b in zip(vec_a, vec_b))
    na = sqrt(sum(a*a for a in vec_a))
    nb = sqrt(sum(b*b for b in vec_b))
    if na == 0.0 or nb == 0.0:
        return 0.0
    return dot / (na * nb)

def _text_sim_ratio(a: str, b: str) -> float:
    """
    Lightweight fallback similarity on normalized text (0..1).
    SequenceMatcher is good enough for MVP; you can swap to rapidfuzz later.
    """
    debug(f"Calculating text similarity between strings of lengths {len(a)} and {len(b)}")

    return SequenceMatcher(None, a, b).ratio()

_NORM_RE = re.compile(r"[^a-z0-9\s]+")

def _normalize_text(s: str) -> str:
    debug(f"Normalizing text of length {len(s)}")

    s = unicodedata.normalize("NFKD", s).encode("ascii", "ignore").decode("ascii")
    s = s.lower()
    s = _NORM_RE.sub(" ", s)
    s = " ".join(s.split())
    return s

def _sha256(s: str) -> str:
    debug(f"Hashing text of length {len(s)}")

    return hashlib.sha256(s.encode("utf-8")).hexdigest()


class BaseQuizManager(ABC):

    
    @abstractmethod
    def get_topics(self) -> List[str]:
        raise NotImplementedError("Subclasses must implement this method.")
    
    @abstractmethod
    def get_input(self):
        raise NotImplementedError("Subclasses must implement this method.")
    
    @abstractmethod
    async def generate_context(self, input_data, amount_question):
        raise NotImplementedError("Subclasses must implement this method.")
    
    @abstractmethod
    def get_questions(self):
        raise NotImplementedError("Subclasses must implement this method.")
    
    @abstractmethod
    def save_questions(self):
        raise NotImplementedError("Subclasses must implement this method.")
    
    def _is_too_similar(
    self,
    *,
    input_id: int,
    candidate_text: str,
    candidate_norm: str,
    cand_threshold: float = 0.71,
    ) -> bool:
        """
        Enforce '>= 70% similar' rule PER INPUT.
        - Prefer cosine similarity on embeddings when both sides have vectors.
        - Fall back to normalized text similarity when embeddings missing.
        """
        existing = self._fetch_existing_questions_for_input(input_id)

        # Try to embed candidate once (if supported). It's OK if this returns None.
        cand_vec = self._embed_question_text(candidate_text)

        for row in existing:
            ex_vec = row.get("embedding")
            ex_norm = (row.get("normalized_text") or "")

            if cand_vec and ex_vec:
                # Vector-based (preferred)
                sim = _cosine_sim(cand_vec, ex_vec)
                if sim >= cand_threshold:
                    return True
            else:
                # Text fallback on normalized text
                # You can tune this threshold separately if you want (e.g., 0.80)
                ts = _text_sim_ratio(candidate_norm, ex_norm)
                if ts >= cand_threshold:
                    return True

        return False
    
    async def _embed_question_text_async(self, text: str) -> list[float] | None:
        try:
            return await self.gemini_client.embed_text(text)
        except Exception as e:
            error(f"Embedding failed: {e}")
            return None

    def _embed_question_text(self, text: str) -> list[float] | None:
        """
        Sync wrapper so you can call it from sync code paths.
        If QuizManager methods are async, prefer the async version directly.
        """
        import asyncio
        try:
            loop = asyncio.get_event_loop()
        except RuntimeError:
            loop = None

        if loop and loop.is_running():
            # You're already in an async context; call async directly from async caller
            # (don’t use this sync wrapper in that case)
            return None

        return asyncio.run(self._embed_question_text_async(text))
    
    def _fetch_existing_questions_for_input(self, input_id: int) -> list[dict]:
        """
        Load existing questions for the given Input with the minimum needed fields.
        """
        rows = self.db_adapter.read_where_many(
            "accredit_question",
            {"input_id": input_id},
        )
        # Keep only what we need
        return [
            {
                "id": r["id"],
                "normalized_text": r.get("normalized_text") or "",
                "embedding": r.get("embedding"),  # may be None
            }
            for r in rows
        ]

    def _is_too_similar(
    self,
    *,
    input_id: int,
    candidate_text: str,
    candidate_norm: str,
    cand_threshold: float = 0.71,
    ) -> bool:
        """
        Enforce '>= 70% similar' rule PER INPUT.
        - Prefer cosine similarity on embeddings when both sides have vectors.
        - Fall back to normalized text similarity when embeddings missing.
        """
        existing = self._fetch_existing_questions_for_input(input_id)

        # Try to embed candidate once (if supported). It's OK if this returns None.
        cand_vec = self._embed_question_text(candidate_text)

        for row in existing:
            ex_vec = row.get("embedding")
            ex_norm = (row.get("normalized_text") or "")

            if cand_vec and ex_vec:
                # Vector-based (preferred)
                sim = _cosine_sim(cand_vec, ex_vec)
                if sim >= cand_threshold:
                    return True
            else:
                # Text fallback on normalized text
                # You can tune this threshold separately if you want (e.g., 0.80)
                ts = _text_sim_ratio(candidate_norm, ex_norm)
                if ts >= cand_threshold:
                    return True

        return False