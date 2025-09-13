# src/dal/remote/enem_adapter.py
from __future__ import annotations

import math
import os
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Dict, List, Tuple

import pandas as pd
import random

from src.dal.remote.base import BaseAdapter
from src.domain.models.preview_model import EnumMode, PreviewModel

try:
    from src.core.settings import app_settings
except Exception:
    app_settings = None


class EnemAdapter(BaseAdapter):
    
    item_name = "enem"
    source_name = "public_and_gov"

    _df: pd.DataFrame | None = None
    _parquet_path: Path | None = None

    # global ordering over the dataset
    _pairs_all: List[Tuple[int, int]] | None = None
    _pairs_ordered: List[Tuple[int, int]] | None = None  # shuffled/sorted once

    def get_preview(self) -> PreviewModel:
        return PreviewModel(
            mode=EnumMode.BOTH,
            source_name=self.source_name,
            has_topic=True,
            item_name=self.item_name,
            item_img="https://res.cloudinary.com/dhncdmb2t/image/upload/v1756293205/ssss_logo_t93flf.png",
            updated_at=datetime.now(timezone.utc).isoformat(),
        )

    # -------- Path resolution (same as before) --------
    def _resolve_parquet_path(self) -> Path:
        if app_settings:
            try:
                s = app_settings()
                val = getattr(s, "ENEM_PARQUET_PATH", None)
                if val:
                    p = Path(val)
                    if not p.is_absolute():
                        p = (Path.cwd() / p).resolve()
                    if p.exists():
                        return p
            except Exception:
                pass

        env_val = os.getenv("ENEM_PARQUET_PATH")
        if env_val:
            p = Path(env_val)
            if not p.is_absolute():
                p = (Path.cwd() / p).resolve()
            if p.exists():
                return p

        here = Path(__file__).resolve()
        candidates = [
            here.parent.parent.parent / "local" / "data" / "enem_questions.parquet",
            Path.cwd() / "data" / "enem_questions.parquet",
        ]
        repo_like = here.parents[3] if len(here.parents) >= 4 else here.parents[-1]
        candidates.append(repo_like / "data" / "enem_questions.parquet")

        for c in candidates:
            c = c.resolve()
            if c.exists():
                return c

        raise FileNotFoundError(
            "Could not locate enem_questions.parquet. "
            "Set ENEM_PARQUET_PATH or generate it with the snapshot script."
        )

    # -------- Load + build global order --------
    def _ensure_loaded(self) -> None:
        if self._df is not None:
            return

        self._parquet_path = self._resolve_parquet_path()
        df = pd.read_parquet(self._parquet_path)

        # required columns
        for col in ("discipline", "dat", "index"):
            if col not in df.columns:
                raise RuntimeError(f"Parquet missing required column: {col}")

        # light dtypes
        df["dat"] = df["dat"].astype("int16", copy=False)
        df["index"] = df["index"].astype("int16", copy=False)
        if df["discipline"].dtype != "category":
            df["discipline"] = df["discipline"].astype("category")

        # build “all pairs” (one per question row)
        pairs_all = list(zip(df["dat"].tolist(), df["index"].tolist()))

        # decide ordering once (shuffle or sort)
        # ENV ENEM_SHUFFLE_SEED:
        #   - integer -> stable shuffle
        #   - "0" or empty -> no shuffle (sorted)
        seed_val = (app_settings().ENEM_SHUFFLE_SEED
                    if app_settings and hasattr(app_settings(), "ENEM_SHUFFLE_SEED")
                    else os.getenv("ENEM_SHUFFLE_SEED", "12345"))
        try:
            seed_int = int(seed_val) if str(seed_val).strip() else 0
        except Exception:
            seed_int = 0

        if seed_int:
            rnd = random.Random(seed_int)
            ordered = pairs_all[:]  # copy
            rnd.shuffle(ordered)
        else:
            # deterministic order without shuffle (by year, then index)
            ordered = sorted(pairs_all)

        self._df = df
        self._pairs_all = pairs_all
        self._pairs_ordered = ordered

    # -------- Public: numeric pagination over all questions --------
    def get_topics(
        self,
        *,
        page: int = 1,
        per_page: int = 30,
        **_: Any
    ) -> Dict[str, Any]:
        """
        Paginates over the entire snapshot (no repeats across pages).
        Returns only: discipline, dat (year), index.
        """
        assert page >= 1 and per_page >= 1
        self._ensure_loaded()
        assert self._df is not None and self._pairs_ordered is not None

        total = len(self._pairs_ordered)              # e.g., 2628
        num_pages = max(1, math.ceil(total / per_page))
        start = (page - 1) * per_page
        end = min(start + per_page, total)

        if start >= total:
            slice_pairs: List[Tuple[int, int]] = []
        else:
            slice_pairs = self._pairs_ordered[start:end]

        # fetch slice, preserving order
        if slice_pairs:
            subset = (
                self._df
                .set_index(["dat", "index"])
                .loc[slice_pairs, ["discipline"]]
                .reset_index()
            )
        else:
            subset = self._df.iloc[0:0][["dat", "index", "discipline"]]

        topics = [
            {"discipline": row["discipline"], "dat": int(row["dat"]), "index": int(row["index"])}
            for _, row in subset.iterrows()
        ]

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
    
    def generate_context():

        ...

    def get_input():

        ...