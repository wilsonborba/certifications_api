from fastapi import APIRouter, Query, Response, status

from src.presentation.handler.topics_handler import get_topics_from_app


from ..handler.responses import MyResponse


app_topics_router = APIRouter()


@app_topics_router.get("/trends/{item_name}", response_model=MyResponse)
async def get_topics(    item_name: str,
    response: Response,
    page: int = Query(1, ge=1),
    per_page: int = Query(45, ge=1, le=300),
    kinds: str | None = Query(None, description="Comma-separated list, e.g. 'top,hot,communities'"),
    time_window: str | None = Query(None, pattern="^(hour|day|week|month|year|all)?$")
):
    response.status_code = status.HTTP_200_OK
    kinds_list = [k.strip().lower() for k in kinds.split(",")] if kinds else None
    topics_data = get_topics_from_app(item_name=item_name, page=page, per_page=per_page, kinds=kinds_list, time_window=time_window
    )
    return MyResponse(
        message=f"Topics for item '{item_name}' retrieved successfully.",
        data=topics_data,
    )
