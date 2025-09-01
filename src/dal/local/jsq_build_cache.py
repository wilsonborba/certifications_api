# tools/jsq_build_cache.py
from __future__ import annotations
import os, re, json, time
from typing import List, Dict, Any, Optional
import requests
import pandas as pd

RAW_URL = "https://raw.githubusercontent.com/lydiahallie/javascript-questions/refs/heads/master/pt-BR/README_pt_BR.md"

OUT_DIR = "data"
OUT_PARQUET = os.path.join(OUT_DIR, "javascript_questions_ptBR.parquet")
OUT_JSONL   = os.path.join(OUT_DIR, "javascript_questions_ptBR.jsonl")

TIMEOUT = 60
RETRIES = 3
SLEEP_BETWEEN_RETRIES = 1.2

HDR_RE = re.compile(r"^######\s+(\d+)\.\s+(.*)$")           # ###### 1. Qual o resultado?
OPTION_RE = re.compile(r"^-\s+([A-Z]):\s*(.*)$")             # - A: ...
ANSWER_RE = re.compile(r"^\s*####\s*Resposta:\s*([A-Z])\s*$", re.I)  # #### Resposta: D
HR_RE = re.compile(r"^---\s*$")                              # '---' separator
CODE_FENCE_RE = re.compile(r"^```(\w+)?\s*$")                # ```javascript

def _get(url: str) -> requests.Response:
    last = None
    for _ in range(RETRIES):
        try:
            r = requests.get(url, timeout=TIMEOUT, headers={"User-Agent": "Asodya-JSQuestions-Snapshot/1.0"})
            if r.status_code == 200: return r
        except Exception as e:
            last = e
        time.sleep(SLEEP_BETWEEN_RETRIES)
    if last: raise last
    raise RuntimeError(f"GET {url} failed")

def parse_markdown(md: str) -> List[Dict[str, Any]]:
    rows: List[Dict[str, Any]] = []
    lines = md.splitlines()

    qnum: Optional[int] = None
    title = None
    in_code = False
    code_lang = None
    code_buf: List[str] = []
    opts: Dict[str, str] = {}
    in_details = False
    details_buf: List[str] = []
    answer_letter: Optional[str] = None

    def flush():
        nonlocal qnum, title, code_buf, code_lang, opts, details_buf, in_details, in_code, answer_letter
        if qnum is None: return
        rows.append({
            "qnum": qnum,
            "title": title,
            "code_lang": code_lang,
            "code": "\n".join(code_buf).strip() if code_buf else None,
            "options_json": json.dumps(opts, ensure_ascii=False) if opts else None,
            "answer_letter": answer_letter,
            "explanation_html": "\n".join(details_buf).strip() if details_buf else None,
            "locale": "pt-BR",
            "source": "lydiahallie/javascript-questions",
        })
        # reset
        qnum = None
        title = None
        in_code = False
        code_lang = None
        code_buf = []
        opts = {}
        in_details = False
        details_buf = []
        answer_letter = None

    for ln in lines:
        # close code fence?
        if in_code:
            if CODE_FENCE_RE.match(ln):
                in_code = False
                code_lang = code_lang or "text"
                continue
            code_buf.append(ln)
            continue

        # detect start code fence
        mcode = CODE_FENCE_RE.match(ln)
        if mcode:
            in_code = True
            code_lang = (mcode.group(1) or "").strip() or None
            continue

        # in explanation (<details>…</details>) capture raw HTML/markdown
        if "<details" in ln:
            in_details = True
            details_buf.append(ln)
            continue
        if in_details:
            details_buf.append(ln)
            if "</details>" in ln:
                in_details = False
            # scan for answer line while in details
            mans = ANSWER_RE.search(ln)
            if mans:
                answer_letter = mans.group(1).strip()
            continue

        # header new question
        mh = HDR_RE.match(ln)
        if mh:
            # if there’s an active question, flush
            flush()
            qnum = int(mh.group(1))
            title = mh.group(2).strip()
            # progress
            print(f"\rAdded {len(rows):,}: Q#{qnum}", end="")
            continue

        # options
        mo = OPTION_RE.match(ln)
        if mo and qnum is not None:
            opts[mo.group(1)] = mo.group(2).strip()
            continue

        # hard separator can also signal end of a block
        if HR_RE.match(ln):
            # don't flush immediately; next header will flush.
            continue

        # other lines are ignored for the compact snapshot

    # flush last
    flush()
    print()  # newline
    return rows

def main():
    os.makedirs(OUT_DIR, exist_ok=True)
    r = _get(RAW_URL)
    rows = parse_markdown(r.text)

    # JSONL (debug)
    pd.Series(rows).to_json(OUT_JSONL, orient="records", lines=True)

    # DataFrame + light dtypes
    df = pd.DataFrame(rows)
    if "qnum" in df: df["qnum"] = df["qnum"].astype("int32")
    for c in ["locale", "source", "code_lang"]:
        if c in df: df[c] = df[c].astype("category")
    for c in ["title", "code", "options_json", "answer_letter", "explanation_html"]:
        if c in df: df[c] = df[c].astype("object")

    df.to_parquet(OUT_PARQUET, index=False)
    print(f"OK! Saved {len(df):,} JS questions to {OUT_PARQUET} and {OUT_JSONL}")

if __name__ == "__main__":
    main()
