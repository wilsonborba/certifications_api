from fastapi import APIRouter, Query, Response, status

from src.presentation.handler.input_handler import get_input_from_app


from ..handler.responses import MyResponse


input_router = APIRouter()

@input_router.get("/input/{item_name}/{input_identification}", response_model=MyResponse)
async def get_input(response: Response, item_name: str, input_identification: str):
    response.status_code = status.HTTP_200_OK
    input_data =  get_input_from_app(item_name=item_name, input_identification=input_identification)
    return MyResponse(
         message=f"Input data for {item_name} with ID {input_identification}",
        data=input_data
        )