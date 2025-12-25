from datetime import datetime, timedelta, timezone

from fastapi import Request, Response  # APIRouter, Query,  status, Body

from src.core.logs import debug
from src.core.utils import get_redis_adapter
from src.dal.local.db_adapter import DBAdapter
from src.domain.services.quiz_api_manager import QuizAPIManager
from src.domain.services.quiz_pdf_manager import QuizPDFManager
from src.presentation.handler.pdf_handler import cache_get

quiz_handler = QuizAPIManager()

quiz_pdf_manager = QuizPDFManager()
db_adapter = DBAdapter()


async def submit_quiz_revision_for_pdf(
    request: Request,
    response: Response,
    answers: list,
    time_spent_seconds: float,
    certification_title: str,
    full_name: str,
    language: str,
    user_uuid_id: str,
    document_id: str,
):
    redis_adapter = get_redis_adapter(request)

    questions = await quiz_pdf_manager.get_questions(
        redis_adapter, document_id=document_id
    )

    cache = request.app.state.redis
    found, input_data, ttl = await cache_get(cache, document_id)

    # it will return a
    # [{'id': 17, 'mode': 'both',
    # 'source_name': 'pdf', 'has_topic': False, 'item_name': 'pdf',
    # 'item_img': 'https://example.com/example.png',
    # 'updated_at': datetime.datetime(2025, 12, 25, 21, 7, 6, 247995, tzinfo=datetime.timezone(datetime.timedelta(seconds=25200)))}]
    itemsource = db_adapter.read_where_one(
        "accredit_sourceitem",
        {"source_name": "pdf", "item_name": "pdf", "has_topic": False},
    )

    # we need get the id of the itemsource to add a

    source_item_id = itemsource["id"]  # itemsource[0]["id"] if itemsource else None

    payload_input = {
        "source_item_id": source_item_id,
        "input_identification": document_id,
        "input_data": input_data,
        "updated_at": datetime.now(timezone.utc).isoformat(),
    }

    inserted_input = db_adapter.insert_row("accredit_input", payload_input)

    saved_questions = quiz_handler.save_questions(
        response={"questions": questions},
        item_name="pdf",
        input_identification=document_id,
        selected_language=language,
    )

    debug(f"Saved questions: {saved_questions}")

    return


async def submit_quiz_revision(
    request: Request,
    response: Response,
    answers: list,
    time_spent_seconds: float,
    certification_title: str,
    full_name: str,
    language: str,
    is_for_pdf: bool,
    user_uuid_id: str,
    document_id: str,
):
    pass

    if is_for_pdf:
        return await submit_quiz_revision_for_pdf(
            request,
            response,
            answers,
            time_spent_seconds,
            certification_title,
            full_name,
            language,
            user_uuid_id,
            document_id,
        )

    # Normal quiz revision processing

    result = quiz_handler.process_quiz_revision(
        answers=answers,
        time_spent_seconds=time_spent_seconds,
        certification_title=certification_title,
        full_name=full_name,
        language=language,
        user_uuid_id=user_uuid_id,
    )

    return result


async def fetch_certification(certification_id):
    result = db_adapter.read_by_id(
        "accredit_usercertification", certification_id, id_column="uuid_certification"
    )

    debug(f"Result: {result}")

    return result
