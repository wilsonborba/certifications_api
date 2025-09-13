# src/dal/remote/sat_adapter.py
from __future__ import annotations

import math
import os
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Dict, List

import pandas as pd
import random

from src.dal.remote.base import BaseAdapter
from src.domain.models.preview_model import EnumMode, PreviewModel

try:
    from src.core.settings import app_settings
except Exception:
    app_settings = None

class SatAdapter(BaseAdapter):
    item_name = "sat"
    source_name = "public_and_gov"

    _df: pd.DataFrame | None = None
    _parquet_path: Path | None = None
    _order: List[str] | None = None   # list of uids in global order

    # ---------- preview ----------
    def get_preview(self) -> PreviewModel:
        return PreviewModel(
            mode=EnumMode.PLAYFUL,
            source_name=self.source_name,
            has_topic=True,
            item_name=self.item_name,
            item_img="https://res.cloudinary.com/dhncdmb2t/image/upload/v1756293205/reddit_logo_t93flf.png",
            updated_at=datetime.now(timezone.utc).isoformat(),
        )

    # ---------- path resolution ----------
    def _resolve_parquet_path(self) -> Path:
        # 1) settings
        if app_settings:
            try:
                s = app_settings()
                val = getattr(s, "SAT_PARQUET_PATH", None)
                if val:
                    p = Path(val)
                    if not p.is_absolute():
                        p = (Path.cwd() / p).resolve()
                    if p.exists():
                        return p
            except Exception:
                pass

        # 2) env
        env_val = os.getenv("SAT_PARQUET_PATH")
        if env_val:
            p = Path(env_val)
            if not p.is_absolute():
                p = (Path.cwd() / p).resolve()
            if p.exists():
                return p

        # 3) fallbacks relative to this file
        here = Path(__file__).resolve()
        candidates = [
            here.parent.parent.parent / "local" / "data" / "sat_questions.parquet",
            Path.cwd() / "data" / "sat_questions.parquet",
            (here.parents[3] if len(here.parents) >= 4 else here.parents[-1]) / "data" / "sat_questions.parquet",
        ]
        for c in candidates:
            c = c.resolve()
            if c.exists():
                return c

        raise FileNotFoundError("sat_questions.parquet not found. Set SAT_PARQUET_PATH or generate it with sat_build_cache.py")

    # ---------- load + build global order ----------
    def _ensure_loaded(self) -> None:
        if self._df is not None:
            return
        self._parquet_path = self._resolve_parquet_path()
        df = pd.read_parquet(self._parquet_path)

        # required columns
        req = ["uid", "module", "skill_desc", "difficulty"]
        for c in req:
            if c not in df.columns:
                raise RuntimeError(f"Parquet missing required column: {c}")

        # categories already set by builder; ensure minimal types
        df["uid"] = df["uid"].astype("string")

        # Build a deterministic global order (shuffle w/ seed or sort)
        if app_settings and hasattr(app_settings(), "SAT_SHUFFLE_SEED"):
            seed_val = getattr(app_settings(), "SAT_SHUFFLE_SEED")
        else:
            seed_val = os.getenv("SAT_SHUFFLE_SEED", "12345")

        try:
            seed_int = int(str(seed_val).strip()) if str(seed_val).strip() else 0
        except Exception:
            seed_int = 0

        uids = df["uid"].tolist()
        if seed_int:
            rnd = random.Random(seed_int)
            order = uids[:]
            rnd.shuffle(order)
        else:
            # Stable order by (module, skill_cd, difficulty, questionId, uid)
            by_cols = [c for c in ["module", "skill_cd", "difficulty", "questionId", "uid"] if c in df.columns]
            order = df.sort_values(by=by_cols, na_position="last")["uid"].tolist()

        self._df = df.set_index("uid", drop=False)
        self._order = order

    # ---------- public ----------
    def get_topics(
        self,
        *,
        page: int = 1,
        per_page: int = 30,
        **_: Any
    ) -> Dict[str, Any]:
        """
        Paginates over all SAT items (no repeats).
        Return a *light* topic tuple. You can choose which fields to expose.
        """
        assert page >= 1 and per_page >= 1
        self._ensure_loaded()
        assert self._df is not None and self._order is not None

        total = len(self._order)
        num_pages = max(1, math.ceil(total / per_page))
        start = (page - 1) * per_page
        end = min(start + per_page, total)
        slice_uids = self._order[start:end] if start < total else []

        # Pick the minimal fields for “topic” (customize as needed)
        cols = [c for c in ["uid", "questionId", "module", "primary_class_cd_desc", "skill_desc", "difficulty"] if c in self._df.columns]
        subset = self._df.loc[slice_uids, cols] if slice_uids else self._df.iloc[0:0][cols]

        topics = []
        for _, row in subset.iterrows():
            topics.append({
                # keep this minimal; you asked “just the simple topic”
                "uid": row.get("uid"),
                "module": row.get("module"),
                "skill": row.get("skill_desc"),
                "difficulty": row.get("difficulty"),
                "qid": row.get("questionId"),
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

    # Optional: get full record by uid (for details page)
    def get_question(self, uid: str) -> Dict[str, Any]:
        self._ensure_loaded()
        if uid in self._df.index:
            return self._df.loc[uid].to_dict()
        return {}

    def generate_context():

        ...

    def get_input():

        ...