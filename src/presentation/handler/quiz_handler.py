
from fastapi import Request, Response # APIRouter, Query,  status, Body

from src.core.utils import get_redis_adapter
from src.domain.services.quiz_pdf_manager import QuizPDFManager
from src.domain.services.quiz_api_manager import QuizAPIManager
from src.core.logs import debug

quiz_handler = QuizAPIManager()

quiz_pdf_manager = QuizPDFManager()

async def submit_quiz_revision_for_pdf(
    request: Request,
    response: Response,
    answers: list, 
    time_spent_seconds: float, 
    certification_title: str, 
    full_name: str,
    language: str,
    user_uuid_id: str,
    document_id: str | None = None
    ):
    redis_adapter = get_redis_adapter(request)

    questions = await quiz_pdf_manager.get_questions(redis_adapter, document_id=document_id)

    for q in questions:
        pdf_question_id = q.get("pdf_question_id")
        

    debug(f"PDF Questions retrieved for revision: {questions}")

    return 



def submit_quiz_revision(
    request: Request,
    response: Response,
    answers: list, 
    time_spent_seconds: float, 
    certification_title: str, 
    full_name: str, 
    language: str, 
    is_for_pdf: bool,
    user_uuid_id: str,
    document_id: str | None = None
    ):
    pass

    if is_for_pdf:
        return submit_quiz_revision_for_pdf(
            request,
            response,
            answers,
            time_spent_seconds,
            certification_title,
            full_name,
            language,
            user_uuid_id,
            document_id
        )
    
    # Normal quiz revision processing

    result = quiz_handler.process_quiz_revision(
        answers=answers,
        time_spent_seconds=time_spent_seconds,
        certification_title=certification_title,
        full_name=full_name,
        language=language,
        user_uuid_id=user_uuid_id
    )

    return result



