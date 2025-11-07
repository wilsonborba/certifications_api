
from fastapi import APIRouter, Query, Request, Response, status, Body

from src.presentation.handler.quiz_handler import submit_quiz_revision
from src.core.logs import debug, error

from ..handler.responses import MyResponse

quiz_router = APIRouter()

@quiz_router.post("/quiz/revision", response_model=MyResponse)
async def get_quiz_revision(
    request: Request,
    response: Response,
    body: dict = Body(...),
    ):
    
    debug("Received request for quiz revision")

    # Placeholder implementation

    # THE BODY EXAMPLE

    # {'answers':
    #  [
    # {'questionId': '280', 'selectedIndex': None, 'selectedText': None}, 
    # {'questionId': '279', 'selectedIndex': None, 'selectedText': None}, 
    # {'questionId': '281', 'selectedIndex': None, 'selectedText': None},
    #  {'questionId': '284', 'selectedIndex': None, 'selectedText': None},
    #  {'questionId': '288', 'selectedIndex': None, 'selectedText': None}, 
    # ...
    # ], 
    # 'time_spent_seconds': 11, 
    # 'certification_title': 'Test', 
    # 'full_name': 'Wilson Borba', 
    # 'language': 'English',
    # 'is_for_pdf': False
    # }
    user_uuid_id = request.headers.get("x-uuid")

    answers = body.get("answers", [])
    time_spent_seconds = body.get("time_spent_seconds", 0)
    certification_title = body.get("certification_title", "")
    full_name = body.get("full_name", "")
    language = body.get("language", "")
    is_for_pdf = body.get("is_for_pdf", False)

    result_quiz = submit_quiz_revision(
        request, response,
        answers, time_spent_seconds, 
        certification_title, full_name, 
        language, is_for_pdf,
        user_uuid_id
    )
    response.status_code = status.HTTP_200_OK
    return MyResponse(data=result_quiz, message="Quiz revision successful")