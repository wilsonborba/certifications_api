# src/dal/remote/jsq_adapter.py
from __future__ import annotations

import math, os, random
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Dict, List

import pandas as pd
from src.dal.remote.base import BaseAdapter
from src.domain.models.preview_model import EnumMode, PreviewModel

try:
    from src.core.settings import app_settings
except Exception:
    app_settings = None

class JsQuestionsAdapter(BaseAdapter):
    item_name = "js-questions"
    source_name = "encyclopedic"

    _df: pd.DataFrame | None = None
    _order: List[int] | None = None
    _parquet_path: Path | None = None

    def get_preview(self) -> PreviewModel:
        return PreviewModel(
            mode=EnumMode.SERIOUS,
            source_name=self.source_name,
            has_topic=True,
            item_name=self.item_name,
            item_img="https://res.cloudinary.com/dhncdmb2t/image/upload/v1756293205/reddit_logo_t93flf.png",
            updated_at=datetime.now(timezone.utc).isoformat(),
        )

    def _resolve_parquet(self) -> Path:
        # settings
        if app_settings:
            try:
                s = app_settings()
                val = getattr(s, "JSQ_PARQUET_PATH", None)
                if val:
                    p = Path(val)
                    if not p.is_absolute(): p = (Path.cwd() / p).resolve()
                    if p.exists(): return p
            except Exception:
                pass
        # env
        env = os.getenv("JSQ_PARQUET_PATH")
        if env:
            p = Path(env)
            if not p.is_absolute(): p = (Path.cwd() / p).resolve()
            if p.exists(): return p
        # fallbacks
        here = Path(__file__).resolve()
        for c in [
            here.parent.parent.parent / "local" / "data" / "javascript_questions_ptBR.parquet",
            Path.cwd() / "data" / "javascript_questions_ptBR.parquet",
        ]:
            c = c.resolve()
            if c.exists(): return c
        raise FileNotFoundError("javascript_questions_ptBR.parquet not found. Set JSQ_PARQUET_PATH or run jsq_build_cache.py")

    def _ensure_loaded(self) -> None:
        if self._df is not None: return
        self._parquet_path = self._resolve_parquet()
        df = pd.read_parquet(self._parquet_path)
        if "qnum" not in df.columns or "title" not in df.columns:
            raise RuntimeError("Parquet missing required columns: qnum/title")
        df["qnum"] = df["qnum"].astype("int32", copy=False)
        self._df = df.set_index("qnum", drop=False)

        # build global order once (shuffle via seed or sort by qnum)
        seed = None
        if app_settings and hasattr(app_settings(), "JSQ_SHUFFLE_SEED"):
            seed = getattr(app_settings(), "JSQ_SHUFFLE_SEED")
        else:
            seed = os.getenv("JSQ_SHUFFLE_SEED", "0")
        try:
            seed_int = int(str(seed).strip()) if str(seed).strip() else 0
        except Exception:
            seed_int = 0

        order = self._df["qnum"].tolist()
        if seed_int:
            rnd = random.Random(seed_int)
            rnd.shuffle(order)
        else:
            order.sort()
        self._order = order

    def get_topics(self, *, page: int = 1, per_page: int = 30, **_: Any) -> Dict[str, Any]:
        assert page >= 1 and per_page >= 1
        self._ensure_loaded()
        assert self._df is not None and self._order is not None

        total = len(self._order)
        num_pages = max(1, math.ceil(total / per_page))
        start = (page - 1) * per_page
        end = min(start + per_page, total)
        qnums = self._order[start:end] if start < total else []

        cols = [c for c in ["qnum", "title", "code_lang"] if c in self._df.columns]
        subset = self._df.loc[qnums, cols] if qnums else self._df.iloc[0:0][cols]

        topics = []
        for _, row in subset.iterrows():
            topics.append({
                "qid": int(row["qnum"]),
                "title": row.get("title"),
                "lang": row.get("code_lang"),
            })

        return {
            "topics": topics,
            "page": page,
            "per_page": per_page,
            "has_more": page < num_pages,
            "total": total,
            "updated_at": datetime.now(timezone.utc).isoformat(),
            "item_name": self.item_name,
            "source_name": self.source_name,
        }

    # optional: full record (for details view)
    def get_question(self, qnum: int) -> Dict[str, Any]:
        self._ensure_loaded()
        if qnum in self._df.index:
            return self._df.loc[qnum].to_dict()
        return {}
