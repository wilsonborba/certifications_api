# tools/sat_build_cache.py
from __future__ import annotations
import os
import sys
import time
import json
import math
from typing import Any, Dict, List, Tuple, Optional
import requests
import pandas as pd

# Optional: streaming parser (handles huge JSON without loading all at once)
# pip install ijson
try:
    import ijson
    HAS_IJSON = True
except Exception:
    HAS_IJSON = False

RAW_URL = "https://raw.githubusercontent.com/Soundwave0/SAT_questions/refs/heads/main/questions.json"

OUT_DIR = "data"
OUT_PARQUET = os.path.join(OUT_DIR, "sat_questions.parquet")
OUT_JSONL   = os.path.join(OUT_DIR, "sat_questions.jsonl")

TIMEOUT = 60
RETRIES = 3
SLEEP_BETWEEN_RETRIES = 1.2

def _download_stream(url: str) -> requests.Response:
    last_exc: Optional[Exception] = None
    for _ in range(RETRIES):
        try:
            r = requests.get(url, stream=True, timeout=TIMEOUT, headers={"User-Agent": "Asodya-SAT-Snapshot/1.0"})
            if r.status_code == 200:
                return r
            # retry on non-200 too
        except Exception as e:
            last_exc = e
        time.sleep(SLEEP_BETWEEN_RETRIES)
    if last_exc:
        raise last_exc
    raise RuntimeError(f"Failed to GET {url}")

def _norm_text(v: Any) -> Optional[str]:
    if v is None:
        return None
    if isinstance(v, str):
        return v
    try:
        return json.dumps(v, ensure_ascii=False)
    except Exception:
        return str(v)

def _flatten_one(uid: str, payload: Dict[str, Any]) -> Dict[str, Any]:
    """
    Flatten the SAT record into stable columns.
    Keeps HTML/MathML as-is in text columns. Also keeps compact JSON for choices.
    """
    c = payload.get("content") or {}
    answer = c.get("answer") or {}

    # normalize multiple possible text holders: stem/prompt/body
    stem = c.get("stem") or c.get("prompt") or c.get("body") or c.get("question") or None
    rationale = c.get("rationale") or answer.get("rationale") or None

    # choices & correctness (MC vs SPR etc.)
    choices_json = None
    correct_choice = None
    correct_answer_list = None
    answer_style = answer.get("style") or c.get("type") or None
    if "choices" in answer:
        try:
            choices_json = json.dumps(answer.get("choices"), ensure_ascii=False)
        except Exception:
            choices_json = None
        correct_choice = answer.get("correct_choice")
    if "correct_answer" in c:
        # usually list of strings
        try:
            correct_answer_list = json.dumps(c.get("correct_answer"), ensure_ascii=False)
        except Exception:
            correct_answer_list = None

    return {
        # identity
        "uid": uid,
        "questionId": payload.get("questionId"),
        "external_id": payload.get("external_id"),
        "program": payload.get("program"),
        "module": payload.get("module"),
        "primary_class_cd": payload.get("primary_class_cd"),
        "primary_class_cd_desc": payload.get("primary_class_cd_desc"),
        "skill_cd": payload.get("skill_cd"),
        "skill_desc": payload.get("skill_desc"),
        "difficulty": payload.get("difficulty"),
        "updateDate": payload.get("updateDate"),
        "createDate": payload.get("createDate"),

        # content
        "section": c.get("section"),
        "type": c.get("type"),             # e.g., "spr"
        "templateid": c.get("templateid"),
        "origin": c.get("origin"),
        "keys_json": json.dumps(c.get("keys") or [], ensure_ascii=False),

        "stem_html": _norm_text(stem),
        "rationale_html": _norm_text(rationale),

        "answer_style": answer_style,
        "choices_json": choices_json,              # dict keyed by a/b/c/d, as JSON
        "correct_choice": correct_choice,          # for MC
        "correct_answer_list": correct_answer_list # for SPR lists
    }

def main() -> None:
    os.makedirs(OUT_DIR, exist_ok=True)
    r = _download_stream(RAW_URL)

    added = 0
    rows: List[Dict[str, Any]] = []

    if HAS_IJSON:
        # Stream as big object: top-level is { "<uuid>": { .. }, ... }
        # ijson.kvitems iterates (key, value) at top-level
        for uid, payload in ijson.kvitems(r.raw, "", use_float=True):
            flat = _flatten_one(uid, payload)
            rows.append(flat)
            added += 1
            # progress (SAT has no year/index; show program + questionId instead)
            info = flat.get("program") or "SAT"
            qid  = flat.get("questionId") or uid
            print(f"\rAdded {added:,}: {info} #{qid}", end="")
    else:
        # Fallback: load entire JSON (uses more memory)
        data = r.json()
        for uid, payload in data.items():
            flat = _flatten_one(uid, payload)
            rows.append(flat)
            added += 1
            info = flat.get("program") or "SAT"
            qid  = flat.get("questionId") or uid
            print(f"\rAdded {added:,}: {info} #{qid}", end="")

    print()  # newline after progress

    # Save JSONL for debugging/portability
    pd.Series(rows).to_json(OUT_JSONL, orient="records", lines=True)

    # Build DataFrame & optimize dtypes
    df = pd.DataFrame(rows)
    # categories for low-cardinality columns
    for col in ["program", "module", "primary_class_cd", "primary_class_cd_desc",
                "skill_cd", "skill_desc", "difficulty", "answer_style", "type", "section"]:
        if col in df.columns:
            df[col] = df[col].astype("category")

    # strings -> keep as object
    for col in ["uid", "questionId", "external_id", "templateid", "origin",
                "stem_html", "rationale_html", "choices_json",
                "correct_choice", "correct_answer_list", "keys_json"]:
        if col in df.columns:
            df[col] = df[col].astype("object")

    # integers that fit in 64-bit (timestamps)
    for col in ["updateDate", "createDate"]:
        if col in df.columns and df[col].notna().any():
            df[col] = pd.to_numeric(df[col], errors="coerce").astype("Int64")

    df.to_parquet(OUT_PARQUET, index=False)
    print(f"OK! Saved {len(df):,} SAT items to {OUT_PARQUET} and {OUT_JSONL}")

if __name__ == "__main__":
    main()
