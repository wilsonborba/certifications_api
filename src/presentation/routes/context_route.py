
from fastapi import APIRouter, Query, Response, status

from src.presentation.handler.context_handler import get_context_from_app


from ..handler.responses import MyResponse

context_router = APIRouter()


@context_router.get("/context/{item_name}/{input_identification}", response_model=MyResponse)
async def get_context(item_name: str, input_identification: str, response: Response):
    context = await get_context_from_app(item_name=item_name, input_identification=input_identification)
    if not context:
        response.status_code = status.HTTP_404_NOT_FOUND
        return MyResponse(data=None, message="Context not found")
    return MyResponse(data=context, message="Context found")
