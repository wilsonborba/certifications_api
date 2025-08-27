from fastapi import APIRouter, Response, status

from src.presentation.handler.source_item_handler import get_all_sources
from ..handler.responses import MyResponse


source_item_router = APIRouter()


@source_item_router.get("/sources", response_model=MyResponse)
async def get_source_item(
    response: Response,
):
    sources = get_all_sources()
    response.status_code = status.HTTP_200_OK
    return MyResponse(
        message="List of source items retrieved successfully.",
        data=sources,
    )