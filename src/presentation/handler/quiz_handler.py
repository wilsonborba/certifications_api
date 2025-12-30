from datetime import datetime, timedelta, timezone
from typing import Any, Dict, List, Tuple

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


def remap_frontend_question_ids_to_db_ids(
    answers: List[Dict[str, Any]],
    saved_questions: List[Dict[str, Any]],
) -> List[Dict[str, Any]]:
    """
    Remapeia answers[*].questionId (pdf_question_id vindo do front)
    para o ID numérico da questão salva no DB (saved_questions[*].id),
    fazendo match por saved_questions[*].pdf_question_id.

    Retorna:
      - new_answers: lista com questionId sobrescrito quando houver match
      - report: métricas e itens não mapeados para debug/monitoramento
    """
    # Mapa: pdf_question_id (uuid) -> question db id (int)
    pdf_to_db: Dict[str, int] = {}
    duplicates: Dict[str, List[int]] = {}

    for q in saved_questions or []:
        pdf_id = q.get("pdf_question_id")
        db_id = q.get("id")
        if not pdf_id or db_id is None:
            continue

        if pdf_id in pdf_to_db and pdf_to_db[pdf_id] != db_id:
            duplicates.setdefault(pdf_id, [pdf_to_db[pdf_id]]).append(db_id)
        pdf_to_db[pdf_id] = db_id

    new_answers: List[Dict[str, Any]] = []
    unmapped: List[Dict[str, Any]] = []
    mapped_count = 0

    for ans in answers or []:
        # Copia para não mutar a lista original (opcional)
        ans_copy = dict(ans)

        front_qid = ans_copy.get("questionId")
        if not front_qid:
            unmapped.append({"reason": "missing_questionId", "answer": ans_copy})
            new_answers.append(ans_copy)
            continue

        db_qid = pdf_to_db.get(front_qid)
        if db_qid is None:
            unmapped.append(
                {"reason": "no_match_for_pdf_question_id", "questionId": front_qid}
            )
            new_answers.append(ans_copy)
            continue

        ans_copy["questionId"] = db_qid
        mapped_count += 1
        new_answers.append(ans_copy)

    report = {
        "total_answers": len(answers or []),
        "total_saved_questions": len(saved_questions or []),
        "mapped": mapped_count,
        "unmapped": unmapped,
        "duplicate_pdf_ids_in_saved_questions": duplicates,
    }

    debug(f"Remap report: {report}")

    return new_answers


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

    debug(f"Questions: {questions}")

    payload_input = {
        "source_item_id": source_item_id,
        "input_identification": document_id,
        "input_data": input_data,
        "updated_at": datetime.now(timezone.utc).isoformat(),
    }

    inserted_input = db_adapter.insert_row("accredit_input", payload_input)

    saved_questions_model = quiz_handler.save_questions(
        response={"questions": questions},
        item_name="pdf",
        input_identification=document_id,
        selected_language=language,
    )

    saved_questions = getattr(saved_questions_model, "saved_questions", None) or []

    debug(f"Saved questions: {saved_questions_model}")

    answers_mapped = remap_frontend_question_ids_to_db_ids(
        answers=answers,
        saved_questions=saved_questions,
    )

    result = quiz_handler.process_quiz_revision(
        answers=answers_mapped,
        time_spent_seconds=time_spent_seconds,
        certification_title=certification_title,
        full_name=full_name,
        language=language,
        user_uuid_id=user_uuid_id,
    )

    return result


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
