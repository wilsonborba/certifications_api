from fastapi import FastAPI
from fastapi.middleware.cors import CORSMiddleware
from src.presentation.routes.source_item_route import source_item_router
from src.presentation.routes.trends_route import app_trends_router




app = FastAPI(root_path="/", root_path_in_servers=False, redirect_slashes=True)






app.add_middleware(
    CORSMiddleware,
    allow_origins=["*"],
    allow_credentials=True,
    allow_methods=["*"],
    allow_headers=["*"],
)

app.include_router(
    source_item_router,
    tags=["source_items"]
)

app.include_router(
    app_trends_router,
    tags=["app_trends"]
)