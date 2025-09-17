from fastapi import FastAPI
from fastapi.middleware.cors import CORSMiddleware
from src.presentation.routes.source_item_route import source_item_router
from src.presentation.routes.topics_route import app_topics_router
from src.presentation.routes.input_route import input_router
from src.presentation.routes.context_route import context_router
from src.presentation.routes.pdf_route import pdf_router




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
    app_topics_router,
    tags=["app_topics"]
)

app.include_router(
    input_router,
    tags=["input_from_topic"]
)

app.include_router(
    context_router,
    tags=["context"]
)

app.include_router(
    pdf_router,
    tags=["pdf"]
)