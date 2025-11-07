
from fastapi import Request, Response # APIRouter, Query,  status, Body

from src.domain.services.quiz_api_manager import QuizAPIManager
quiz_handler = QuizAPIManager()



def submit_quiz_revision_for_pdf(
    request: Request,
    response: Response,
    answers: list, 
    time_spent_seconds: float, 
    certification_title: str, 
    full_name: str,
    language: str,
    user_uuid_id: str
    ):
    pass



def submit_quiz_revision(
    request: Request,
    response: Response,
    answers: list, 
    time_spent_seconds: float, 
    certification_title: str, 
    full_name: str, 
    language: str, 
    is_for_pdf: bool,
    user_uuid_id: str
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
            user_uuid_id
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



