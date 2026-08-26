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
    *,
    drop_unmapped: bool = True,
) -> List[Dict[str, Any]]:
    """
    Troca answers[*].questionId (pdf_question_id do front) por saved_questions[*].id (BIGINT do DB).
    Match: ans.questionId == saved_question.pdf_question_id.

    Se drop_unmapped=True:
      - Remove do retorno qualquer answer que não puder ser mapeado.
      - Isso evita InvalidTextRepresentation (UUID em coluna BIGINT) e não penaliza o usuário,
        porque o score passa a considerar apenas questões válidas.

    Retorna: lista de answers prontos para process_quiz_revision.
    """

    # pdf_question_id (str) -> db question id (int)
    pdf_to_db: Dict[str, int] = {}
    duplicates: Dict[str, List[int]] = {}

    for q in saved_questions or []:
        pdf_id = q.get("pdf_question_id")
        db_id = q.get("id")

        if not pdf_id or db_id is None:
            continue

        try:
            db_id_int = int(db_id)
        except (TypeError, ValueError):
            continue

        if pdf_id in pdf_to_db and pdf_to_db[pdf_id] != db_id_int:
            duplicates.setdefault(pdf_id, [pdf_to_db[pdf_id]]).append(db_id_int)

        pdf_to_db[pdf_id] = db_id_int

    remapped: List[Dict[str, Any]] = []
    unmapped: List[Dict[str, Any]] = []

    for ans in answers or []:
        ans_copy = dict(ans)

        front_qid = ans_copy.get("questionId")
        if not front_qid:
            unmapped.append({"reason": "missing_questionId", "answer": ans_copy})
            if not drop_unmapped:
                # Se você realmente quiser manter, zera com None para evitar BIGINT error.
                # (Mas note: isso pode causar FK/consulta vazia dependendo do seu adapter.)
                ans_copy["questionId"] = None
                remapped.append(ans_copy)
            continue

        db_qid = pdf_to_db.get(front_qid)
        if db_qid is None:
            unmapped.append(
                {"reason": "no_match_for_pdf_question_id", "questionId": front_qid}
            )
            if not drop_unmapped:
                ans_copy["questionId"] = None
                remapped.append(ans_copy)
            continue

        ans_copy["questionId"] = db_qid  # BIGINT seguro
        remapped.append(ans_copy)

    report = {
        "total_answers_in": len(answers or []),
        "total_saved_questions": len(saved_questions or []),
        "mapped": len(remapped),
        "unmapped_dropped" if drop_unmapped else "unmapped_kept_as_null": len(unmapped),
        "unmapped_samples": unmapped[:5],
        "duplicate_pdf_ids_in_saved_questions": duplicates,
    }
    debug(f"Remap report: {report}")

    return remapped


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
        "certifications_sourceitem",
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

    inserted_input = db_adapter.insert_row("certifications_input", payload_input)

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

    raise ValueError("Only PDF quiz revisions are supported.")


async def fetch_certification(certification_id):
    result = db_adapter.read_by_id(
        "certifications_usercertification", certification_id, id_column="uuid_certification"
    )

    debug(f"Result: {result}")

    return result


async def fetch_certification_from_user(user_uuid_id: str) -> List[Dict[str, Any]]:
    results = db_adapter.read_where_many(
        "certifications_usercertification",
        {"user_uuid_id": user_uuid_id},
    )

    return results
