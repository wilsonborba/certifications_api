



from fastapi import UploadFile

from src.presentation.handler.responses import MalwareDetectedError, UnsupportedFileTypeError
from src.dal.local.pdf_adapter import PdfAdapter, PdfParser
from src.core.logs import error
from typing import List, Dict, Any, Optional, Tuple

from src.dal.local.redis_adapter import RedisAdapter

CACHE_PREFIX = "topic:"
CACHE_TTL_SEC = 30 * 60  # 30 minutes

async def cache_get(cache: RedisAdapter, document_id: str) -> Tuple[bool, Optional[Dict[str, Any]], int]:
    """
    Returns (found, payload, ttl).
    ttl semantics from Redis:
      >0   remaining seconds
      -1   no expiration (treat as valid)
      -2   key does not exist
    """
    key = cache.k(CACHE_PREFIX, document_id)
    payload = await cache.get(key)
    if payload is None:
        return False, None, -2
    ttl = await cache.ttl(key)
    return True, payload, ttl

async def cache_set(cache: RedisAdapter, document_id: str, payload: Dict[str, Any]) -> None:
    key = cache.k(CACHE_PREFIX, document_id)
    await cache.set(key, payload, ex=CACHE_TTL_SEC)

def parse_page_selector(select: str | None, total_pages: int) -> List[int]:
    """
    Accepts formats like:
      - "all" or "" -> all pages
      - "1,3,5-7"
      - "2-"
      - "-4"
    Returns 1-based unique sorted pages, clamped to [1..total_pages].
    """
    if not select or select.strip().lower() == "all":
        return list(range(1, total_pages + 1))

    pages: set[int] = set()
    for part in select.split(","):
        part = part.strip()
        if not part:
            continue
        if "-" in part:
            a, b = part.split("-", 1)
            a = a.strip(); b = b.strip()
            start = int(a) if a else 1
            end   = int(b) if b else total_pages
            if start > end:
                start, end = end, start
            for p in range(start, end + 1):
                if 1 <= p <= total_pages:
                    pages.add(p)
        else:
            try:
                p = int(part)
                if 1 <= p <= total_pages:
                    pages.add(p)
            except ValueError:
                # ignore junk silently or raise — here we raise to be strict
                raise ValueError(f"Invalid page token: '{part}'")
    return sorted(pages) or []

def filter_payload_by_pages(payload: Dict[str, Any], pages: List[int]) -> Dict[str, Any]:
    """
    Given the cached payload (exact structure you return from /pdf/ingest),
    keep only the requested pages in:
      - parsed.elements (by el['page'])
      - parsed.chunks (include chunk if any overlap with requested pages)
    """
    parsed = payload.get("parsed", {})
    elements = parsed.get("elements", [])
    chunks   = parsed.get("chunks", [])

    # filter elements
    el_sel = [el for el in elements if int(el.get("page") or 0) in pages]

    # filter chunks: include if any page overlaps
    ch_sel = []
    for ch in chunks:
        ch_pages = ch.get("pages") or []
        if any(int(p) in pages for p in ch_pages):
            ch_sel.append(ch)

    out = dict(payload)
    out["parsed"] = dict(parsed)
    out["parsed"]["elements"] = el_sel
    out["parsed"]["chunks"]   = ch_sel
    out["selection"] = {
        "requested_pages": pages,
        "total_pages": int(payload.get("metadata", {}).get("pages") or 0),
        "elements_count": len(el_sel),
        "chunks_count": len(ch_sel),
    }
    return out


async def  get_topic_from_pdf(
    file: UploadFile,
    ocr_force: bool = False,
    ocr_lang: str = "eng",
    ocr_dpi: int = 300,
    max_chars: int = 8000,
    overlap_chars: int = 400,
):
    if file.content_type and file.content_type not in ("application/pdf", "application/octet-stream"):
        raise UnsupportedFileTypeError("Unsupported file type. Please upload a PDF file.")

    raw = await file.read() 

    # 1) Adapt: validate + metadata + scan detection
    adapter = PdfAdapter(raw, file.filename)
    adapter.validate()

    av_result = adapter.av_scan_clamav()                     # {"status": CLEAN|FOUND|UNKNOWN, ...}
    yara_result = adapter.yara_scan(rules_path=None)         # set a rules file if you have one

    if av_result.get("status") == "FOUND":
        error(f"Malware detected by {av_result['engine']}: {av_result.get('signature')}")
        raise MalwareDetectedError("Malware detected...")

    if yara_result.get("matches"):
        error(f"Malware detected by YARA: {', '.join(yara_result.get('matches', []))}")
        raise MalwareDetectedError("Malware detected...")
        
        

    meta = adapter.get_metadata()
    scan_score = adapter.scan_ratio()

    # 2) Parse: structure + OCR (auto-enable if likely scanned or user forces)
    parser = PdfParser(adapter.as_bytes_io(), adapter.filename)
    parsed = parser.parse(
        ocr_force=ocr_force or (scan_score >= 0.6),
        ocr_lang=ocr_lang,
        ocr_dpi=ocr_dpi,
        max_chars=max_chars,
        overlap_chars=overlap_chars,
    )

    payload = {
        "metadata": {**meta, "scan_ratio": round(scan_score, 3)},
        "parsed": parsed,  # contains: document_id, source, elements, chunks
    }

    return payload