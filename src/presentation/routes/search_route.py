from __future__ import annotations

from fastapi import APIRouter, Query, Response, status
from typing import Optional
from src.presentation.handler.search_handler import search_from_app
from ..handler.responses import MyResponse
from src.core.logs import error

search_router = APIRouter()

@search_router.get("/search/{item_name}", response_model=MyResponse)
async def search(
    item_name: str,
    response: Response,
    q: str = Query(..., min_length=1, description="search query"),
    page: int = Query(1, ge=1),
    per_page: int = Query(20, ge=1, le=300),
    mode: str = Query("fulltext", regex="^(fulltext|substring|fuzzy)$"),
    time_window: Optional[str] = Query(None, description="hour|day|week|month|year|all"),
    fill_page: bool = Query(True),
    max_extra_pages: int = Query(2, ge=0, le=5),
):
    """
    /search keeps the same envelope as /topics:
    {
      item_name, source_name, page, per_page, has_more, updated_at, topics: [ { identifications: IdentificationsModel, ... } ]
    }
    Notes:
    - 'mode' controls push-down only (fulltext) vs local filter (substring/fuzzy).
    - 'time_window' is passed to the adapter when supported (e.g. Reddit).
    - 'fill_page' tries to complete per_page after filtering, bounded by 'max_extra_pages'.
    """
    try:
        payload = search_from_app(
            item_name=item_name,
            q=q,
            page=page,
            per_page=per_page,
            mode=mode,                 # validated by regex above
            time_window=time_window,
            fill_page=fill_page,
            max_extra_pages=max_extra_pages,
        )
        response.status_code = status.HTTP_200_OK
        return MyResponse(
            message=f"Search results for '{item_name}' retrieved successfully.",
            data=payload,
        )
    except ValueError as ve:
        error(f"Search value error: {ve}")
        response.status_code = status.HTTP_422_UNPROCESSABLE_ENTITY
        return MyResponse(
            message=f"Invalid search parameters...",
            data=None,
        )
    except TimeoutError as te:
        error(f"Search timeout error: {te}")
        response.status_code = status.HTTP_504_GATEWAY_TIMEOUT
        return MyResponse(
            message=f"Search timed out...",
            data=None,
        )
    except Exception as ex:
        error(f"Unhandled search error: {ex}")
        response.status_code = status.HTTP_500_INTERNAL_SERVER_ERROR
        return MyResponse(
            message=f"Server search error...",
            data=None,
        )
