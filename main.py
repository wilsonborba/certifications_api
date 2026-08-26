from fastapi import FastAPI
from fastapi.concurrency import asynccontextmanager
from fastapi.middleware.cors import CORSMiddleware

from src.core.settings import app_settings
from src.dal.local.redis_adapter import RedisAdapter
from src.presentation.routes.pdf_route import pdf_router
from src.presentation.routes.quiz_route import quiz_router
from src.presentation.routes.token_route import ai_token_router
from src.presentation.routes.study_route import study_router

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
    title="Certifications API",
    description="API for the Certifications application",
    version="0.1.0",
    lifespan=lifespan,
)


app.add_middleware(
    CORSMiddleware,
    allow_origins=["*"],
    allow_credentials=True,
    allow_methods=["*"],
    allow_headers=["*"],
)

app.include_router(pdf_router, tags=["pdf"])

app.include_router(quiz_router, tags=["quiz"])

app.include_router(ai_token_router, tags=["tokens"])

app.include_router(study_router, tags=["studies"])
