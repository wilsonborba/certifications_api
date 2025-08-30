# tools/enem_build_cache.py
from __future__ import annotations
import json
import math
import random
import time
from dataclasses import dataclass
from typing import Iterable, List, Dict, Any, Tuple
import concurrent.futures as cf

import requests
import pandas as pd

RAW_BASE = "https://raw.githubusercontent.com/yunger7/enem-api/refs/heads/main/public"
YEARS = list(range(2009, 2023 + 1))
INDEX_MIN, INDEX_MAX = 1, 180
OUT_PARQUET = "data/enem_questions.parquet"
OUT_JSONL = "data/enem_questions.jsonl"

# --- ajustes de rede ---
MAX_WORKERS = 16          # paralelo “educado” para o raw.githubusercontent
RETRIES = 3
TIMEOUT = 20
SLEEP_BETWEEN_RETRIES = 1.2

@dataclass(frozen=True)
class Key:
    year: int
    index: int

def url_for(year: int, index: int) -> str:
    # exemplo indicado pelo usuário:
    # https://raw.githubusercontent.com/.../public/2009/questions/30/details.json
    return f"{RAW_BASE}/{year}/questions/{index}/details.json"

def fetch_one(k: Key) -> Tuple[Key, Dict[str, Any] | None]:
    url = url_for(k.year, k.index)
    for attempt in range(1, RETRIES + 1):
        try:
            r = requests.get(url, timeout=TIMEOUT, headers={"User-Agent": "Asodya-ENEM-Snapshot/1.0"})
            if r.status_code == 200:
                data = r.json()
                # mantemos o payload bruto + garantias para os 3 campos essenciais
                return k, {
                    "discipline": data.get("discipline"),
                    "dat": data.get("year", k.year),
                    "index": data.get("index", k.index),
                    # extras úteis (não exigidos pelo adapter, mas bons para futuros usos)
                    "title": data.get("title"),
                    "year": data.get("year", k.year),
                    "language": data.get("language"),
                    "correctAlternative": data.get("correctAlternative"),
                }
            elif r.status_code == 404:
                return k, None  # não existe no repo → ignorar
            else:
                # 5xx/403/etc → retry
                if attempt == RETRIES:
                    return k, None
        except Exception:
            if attempt == RETRIES:
                return k, None
        time.sleep(SLEEP_BETWEEN_RETRIES)
    return k, None

def generate_keys() -> Iterable[Key]:
    for y in YEARS:
        for i in range(INDEX_MIN, INDEX_MAX + 1):
            yield Key(y, i)

def main() -> None:
    keys = list(generate_keys())
    results: List[Dict[str, Any]] = []
    total_added = 0
    with cf.ThreadPoolExecutor(max_workers=MAX_WORKERS) as ex:
        for k, payload in ex.map(fetch_one, keys):
            
            if payload is not None:
                total_added += 1
                print(f"\rAdded {total_added:,}: {k.year} #{k.index}", end="")
                results.append(payload)

    # salva JSONL (debug/portável)
    pd.Series(results).to_json(OUT_JSONL, orient="records", lines=True)

    # salva Parquet (rápido de ler)
    df = pd.DataFrame(results)
    # otimizações leves de dtype
    if "dat" in df:
        df["dat"] = df["dat"].astype("int16")
    if "index" in df:
        df["index"] = df["index"].astype("int16")
    if "discipline" in df:
        df["discipline"] = df["discipline"].astype("category")

    # garanta a pasta data/
    import os
    os.makedirs(os.path.dirname(OUT_PARQUET), exist_ok=True)
    df.to_parquet(OUT_PARQUET, index=False)  # requer pyarrow ou fastparquet

    print(f"OK! Salvo {len(df):,} questões em {OUT_PARQUET} e {OUT_JSONL}")

if __name__ == "__main__":
    main()
