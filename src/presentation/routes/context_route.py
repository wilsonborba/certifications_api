
from fastapi import APIRouter, Query, Request, Response, status

from src.presentation.handler.context_handler import get_context_from_app
from src.core.logs import debug


from ..handler.responses import MyResponse

context_router = APIRouter()


@context_router.get("/context/{item_name}/{input_identification}", response_model=MyResponse)
async def get_context(
    request: Request,
    item_name: str, 
    input_identification: str, 
    response: Response,
    force_new_generation: bool = Query(False, description="Force new generation of questions, ignoring cached ones"),
    amount_question: int = Query(10, ge=1, le=20, description="Number of questions to generate"),
    
    ):


    user_uuid_id = request.headers.get("x-uuid")


    context = await get_context_from_app(
            item_name=item_name, 
            input_identification=input_identification, 
            force_new_generation=force_new_generation,
            amount_question=amount_question,
            user_uuid_id=user_uuid_id
    )
    if not context:
        response.status_code = status.HTTP_404_NOT_FOUND
        return MyResponse(data=None, message="Context not found")
    return MyResponse(data=context, message="Context found")
