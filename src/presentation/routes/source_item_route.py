from fastapi import APIRouter, Response, status

from src.presentation.handler.source_item_handler import get_all_item_data, get_all_sources_data, get_specific_item_data, get_specific_source_data
from ..handler.responses import MyResponse


source_item_router = APIRouter()


@source_item_router.get("/all_sources", response_model=MyResponse)
async def get_all_source(response: Response):
    sources = get_all_sources_data()
    response.status_code = status.HTTP_200_OK
    return MyResponse(
        message="List of source items retrieved successfully.",
        data=sources,
    )

@source_item_router.get("/all_items", response_model=MyResponse)
async def get_all_items(response: Response):
    items = get_all_item_data()
    response.status_code = status.HTTP_200_OK
    return MyResponse(
        message="List of source items retrieved successfully.",
        data=items,
    )



@source_item_router.get("/source/{source_name}", response_model=MyResponse)
async def get_specific_source(source_name: str, response: Response):

    get_specific_source_data(source_name=source_name)
    
    response.status_code = status.HTTP_200_OK
    return MyResponse(
        message=f"Source item '{source_name}' retrieved successfully.",
        data=get_specific_source_data(source_name=source_name),
    )


@source_item_router.get("/item/{item_name}", response_model=MyResponse)
async def get_specific_item(item_name: str, response: Response):

    get_specific_item_data(item_name=item_name)
    
    response.status_code = status.HTTP_200_OK
    return MyResponse(
        message=f"Source item '{item_name}' retrieved successfully.",
        data=get_specific_item_data(item_name=item_name),
    )