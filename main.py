from fastapi import FastAPI
from fastapi.concurrency import asynccontextmanager
from fastapi.middleware.cors import CORSMiddleware
from src.dal.local.redis_adapter import RedisAdapter
from src.presentation.routes.source_item_route import source_item_router
from src.presentation.routes.topics_route import app_topics_router
from src.presentation.routes.input_route import input_router
from src.presentation.routes.context_route import context_router
from src.presentation.routes.pdf_route import pdf_router
from src.presentation.routes.search_route import search_router
from src.presentation.routes.quiz_route import quiz_router
from src.core.settings import app_settings

settings = app_settings()

@asynccontextmanager
async def lifespan(app: FastAPI):
    # Startup
    adapter = RedisAdapter(settings.REDIS_URL, namespace=settings.REDIS_NAMESPACE)
    await adapter.connect()
    app.state.redis = adapter

    yield  # <---- aqui a app roda normalmente

    # Shutdown
    await adapter.close()



app = FastAPI(
    root_path="/", 
    root_path_in_servers=False, 
    redirect_slashes=True,
    title="Accredit API",
    description="API for Accredit application",
    version="0.1.0",
    lifespan=lifespan
    )






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
    search_router,
    tags=["search"]
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

app.include_router(
    quiz_router,
    tags=["quiz"]
)