



import json
from fastapi import Request, UploadFile

from src.core.utils import get_redis_adapter
from src.domain.services.quiz_pdf_manager import QuizPDFManager
from src.presentation.handler.responses import DocumentNotFoundError, InvalidTotalPagesError, MalwareDetectedError, UnsupportedFileTypeError
from src.dal.local.pdf_adapter import PdfAdapter
from src.core.logs import error, debug, info, warning
from typing import List, Dict, Any, Optional, Tuple

from src.dal.local.redis_adapter import RedisAdapter

CACHE_PREFIX = "topic:"
CACHE_TTL_SEC = 30 * 60  # 30 minutes

quiz_pdf_manager = QuizPDFManager()


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
    cache: RedisAdapter,
    ocr_force: bool = False,
    ocr_lang: str = "eng",
    ocr_dpi: int = 300,
    max_chars: int = 8000,
    overlap_chars: int = 400,
):
   

    try:
        topic = await quiz_pdf_manager.get_topics(
            file=file,
            ocr_force=ocr_force,
            ocr_lang=ocr_lang,
            ocr_dpi=ocr_dpi,
            max_chars=max_chars,
            overlap_chars=overlap_chars,
        )

        document_id = topic.get('parsed').get('document_id')
        
        debug(f"Caching topic for document_id {document_id} with TTL {CACHE_TTL_SEC} seconds")

        await cache_set(cache, document_id, topic)

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
    found, input_data, ttl = await cache_get(cache, document_id)
    if not found or ttl == -2:
        raise DocumentNotFoundError("Document ID not found in cache.")            

    total_pages = int(input_data.get("metadata", {}).get("pages") or 0)
    if total_pages <= 0:
        raise InvalidTotalPagesError("Invalid total pages in cached document.")
    
    inputs = quiz_pdf_manager.get_input(
        selected_pages=selected_pages,
        total_pages=total_pages,
        input_data=input_data
    )
    return inputs


async def generate_and_save_questions(
        request: Request,
        document_id: str,
        selected_language: str,
        user_uuid_id: str,
        attempt_index: int,
        selected_pages: str = 'all',
        amount_question: int = 10,
    ) -> dict:

    redis_adapter = get_redis_adapter(request)

    input_data = await get_input_from_pdf(request, document_id, selected_pages)

    response = await quiz_pdf_manager.generate_context(
        input_data=input_data,
        selected_language=selected_language,
        amount_question=amount_question
    )

    status_code = quiz_pdf_manager.pdf_adapter.gemini.last_status_code or 200
    attempts = quiz_pdf_manager.pdf_adapter.gemini.last_attempts or 1
    latency_ms = quiz_pdf_manager.pdf_adapter.gemini.last_latency_ms or 0.0

    user_usage_tracking = quiz_pdf_manager.save_ai_user_usage(
        user_uuid_id=user_uuid_id,
        raw_context=response,
        status_code=status_code,
        attempts=attempts,
        latency_ms=latency_ms,
        source_item_name=None,
        source_input_identification=None,
        is_for_pdf=True
    )

    info(f"User usage tracking saved: {user_usage_tracking}")

    raw_json_str = response['candidates'][0]['content']['parts'][0]['text']
    parsed = json.loads(raw_json_str)
    result = await quiz_pdf_manager.save_questions(
        user_uuid_id=user_uuid_id,
        redis_adapter=redis_adapter,
        response=parsed,
        document_id=document_id,
        amount_question=amount_question,
        attempt_index=attempt_index,
    )

    saved_questions = [q for q in result.saved_questions]

    return saved_questions


async def get_context_from_pdf(
        request: Request, 
        document_id: str, 
        selected_language: str,
        user_uuid_id: str,
        selected_pages: str = 'all', 
        amount_question: int = 10,
        ) -> dict:
    

    try:

        saved_questions = await generate_and_save_questions(
            request=request,
            document_id=document_id,
            selected_language=selected_language,
            user_uuid_id=user_uuid_id,
            selected_pages=selected_pages,
            amount_question=amount_question,
            attempt_index=1
        )

        if len(saved_questions) > amount_question:
            debug(f"More questions saved than requested: {len(saved_questions)} > {amount_question}\n\n{saved_questions}")
            saved_questions = saved_questions[:amount_question]
    
        if len(saved_questions) < amount_question:
            warning(f"Only {len(saved_questions)} questions were saved, less than requested {amount_question}")

            debug(f"Questions so far: {saved_questions}")
        

            new_saved_questions = await generate_and_save_questions(
                request=request,
                document_id=document_id,
                selected_language=selected_language,
                user_uuid_id=user_uuid_id,
                selected_pages=selected_pages,
                amount_question=amount_question,
                attempt_index=2
            )
            
            saved_questions = saved_questions + new_saved_questions


        return saved_questions

    except Exception as e:
        raise e
    


    



