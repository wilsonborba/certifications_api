



from fastapi import Request, UploadFile

from src.presentation.handler.responses import DocumentNotFoundError, InvalidTotalPagesError, MalwareDetectedError, UnsupportedFileTypeError
from src.dal.local.pdf_adapter import PdfAdapter
from src.core.logs import error
from typing import List, Dict, Any, Optional, Tuple

from src.dal.local.redis_adapter import RedisAdapter

CACHE_PREFIX = "topic:"
CACHE_TTL_SEC = 30 * 60  # 30 minutes

pdf_adapter_without_file =  PdfAdapter()


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




async def  get_topic_from_pdf(
    file: UploadFile,
    ocr_force: bool = False,
    ocr_lang: str = "eng",
    ocr_dpi: int = 300,
    max_chars: int = 8000,
    overlap_chars: int = 400,
    cache: RedisAdapter = None,
):
    pdf_adapter = await PdfAdapter.from_upload(file)

    try:
        topic = pdf_adapter.get_topics(
            ocr_force=ocr_force,
            ocr_lang=ocr_lang,
            ocr_dpi=ocr_dpi,
            max_chars=max_chars,
            overlap_chars=overlap_chars,
        )

        await cache_set(cache, pdf_adapter.document_id, topic)

        return topic
    
    except UnsupportedFileTypeError as e:
        error(f"Unsupported file type: {e}")
        raise e
    except MalwareDetectedError as e:
        raise e
    except Exception as e:
        error(f"Error during PDF topic extraction: {e}")
        raise ValueError("Failed to extract topics from PDF.")
    
async def get_input_from_pdf(request: Request, document_id: str, selected_pages: str = 'all'):
    
    
    cache = request.app.state.redis
    found, cached, ttl = await cache_get(cache, document_id)
    if not found or ttl == -2:
        raise DocumentNotFoundError("Document ID not found in cache.")            

    total_pages = int(cached.get("metadata", {}).get("pages") or 0)
    if total_pages <= 0:
        raise InvalidTotalPagesError("Invalid total pages in cached document.")
    
    inputs = pdf_adapter_without_file.get_input(
        selected_pages=selected_pages,
        total_pages=total_pages,
        cached=cached
    )
    return inputs


async def get_context_from_pdf(request: Request, document_id: str, selected_pages: str = 'all'):
    
    inputs = await get_input_from_pdf(request, document_id, selected_pages)

    ai_injection_result =  await pdf_adapter_without_file.check_ai_injection(cached=inputs)

    return ai_injection_result



