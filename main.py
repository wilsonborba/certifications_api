from contextlib import asynccontextmanager

from fastapi import FastAPI, Request
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import JSONResponse
from redis.exceptions import RedisError

from src.core.logs import error
from src.core.settings import app_settings
from src.dal.local.redis_adapter import RedisAdapter, RedisAdapterError
from src.presentation.routes.study_route import study_router
from src.presentation.routes.question_route import question_router
from src.presentation.routes.study_lifecycle_route import lifecycle_router
from src.presentation.routes.waitlist_route import waitlist_router
from src.presentation.routes.quiz_governance_route import quiz_router

settings = app_settings()


@asynccontextmanager
async def lifespan(app: FastAPI):
    adapter = RedisAdapter(settings.REDIS_URL, namespace=settings.REDIS_NAMESPACE)
    try:
        await adapter.connect()
        await adapter.ping()
    except RedisAdapterError as exc:
        await adapter.close()
        error("Redis startup connectivity check failed")
        raise RuntimeError("Certifications state store is unavailable") from exc

    app.state.redis = adapter

    yield

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


@app.exception_handler(RedisAdapterError)
@app.exception_handler(RedisError)
async def redis_unavailable(_: Request, exc: Exception) -> JSONResponse:
    """Keep infrastructure details in server logs and return a stable API error."""
    error(f"Redis request failure: {type(exc).__name__}")
    return JSONResponse(
        status_code=503,
        content={"detail": "Study service is temporarily unavailable. Please try again shortly."},
    )

app.include_router(study_router, tags=["studies"])

app.include_router(question_router, tags=["questions"])

app.include_router(lifecycle_router, tags=["study lifecycle"])

app.include_router(waitlist_router, tags=["waitlist"])

app.include_router(quiz_router, tags=["quizzes"])
