from fastapi import APIRouter, Query, Request, Response, status

from src.presentation.handler.topics_handler import get_topics_from_app


from ..handler.responses import MyResponse
from src.core.logs import debug


app_topics_router = APIRouter()


@app_topics_router.get("/topics/{item_name}", response_model=MyResponse)
async def get_topics(    item_name: str,
    response: Response,
    page: int = Query(1, ge=1),
    per_page: int = Query(45, ge=1, le=300),
):
    response.status_code = status.HTTP_200_OK
    topics_data = get_topics_from_app(item_name=item_name, page=page, per_page=per_page,
    )
    return MyResponse(
        message=f"Topics for item '{item_name}' retrieved successfully.",
        data=topics_data,
    )

@app_topics_router.post("/topics/solicitate_new", response_model=MyResponse)
async def add_new_topic_request(
    response: Response,
    request: Request,
    ):
    response.status_code = status.HTTP_201_CREATED
    # Logic to add a new topic request

    # get from body the website_url from request
    body = await request.json()
    
    website_url = body.get("url", "")



    debug(f"New topic request received from {request.headers["x-uuid"]}  with website URL: {website_url}")

    return MyResponse(
        message=f"New topic request added successfully.",
        data={"website_url": website_url},
    )