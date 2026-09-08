from contextlib import asynccontextmanager

from fastapi import FastAPI, Request
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import JSONResponse
from redis.exceptions import RedisError

from src.core.logs import LogTarget, configure_logging, error
from src.core.settings import app_settings
from src.dal.local.redis_adapter import RedisAdapter, RedisAdapterError
from src.presentation.routes.logs_stream_route import logs_router
from src.presentation.routes.study_route import study_router
from src.presentation.routes.question_route import question_router
from src.presentation.routes.study_lifecycle_route import lifecycle_router
from src.presentation.routes.waitlist_route import waitlist_router
from src.presentation.routes.quiz_governance_route import quiz_router

settings = app_settings()


@asynccontextmanager
async def lifespan(app: FastAPI):
    configure_logging(target=LogTarget.API, log_file=settings.LOG_FILE)
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


import uuid
from starlette.middleware.base import BaseHTTPMiddleware
from starlette.exceptions import HTTPException as StarletteHTTPException
from fastapi.exceptions import RequestValidationError

app.add_middleware(
    CORSMiddleware,
    allow_origins=["*"],
    allow_credentials=True,
    allow_methods=["*"],
    allow_headers=["*"],
)


class RequestIdMiddleware(BaseHTTPMiddleware):
    async def dispatch(self, request: Request, call_next):
        request_id = request.headers.get("x-request-id") or str(uuid.uuid4())
        request.state.request_id = request_id
        response = await call_next(request)
        response.headers["X-Request-ID"] = request_id
        return response


app.add_middleware(RequestIdMiddleware)


def _extract_request_id(request: Request | None) -> str | None:
    if request is None:
        return None
    req_state = getattr(request, "state", None)
    if req_state:
        req_id = getattr(req_state, "request_id", None)
        if req_id:
            return req_id
    headers = getattr(request, "headers", None)
    if headers:
        return headers.get("x-request-id")
    return None


@app.exception_handler(StarletteHTTPException)
async def http_exception_handler(request: Request, exc: StarletteHTTPException) -> JSONResponse:
    request_id = _extract_request_id(request)
    content = {"detail": exc.detail}
    if request_id:
        content["request_id"] = request_id
    headers = {"X-Request-ID": request_id} if request_id else {}
    return JSONResponse(
        status_code=exc.status_code,
        content=content,
        headers=headers,
    )


@app.exception_handler(RequestValidationError)
async def validation_exception_handler(request: Request, exc: RequestValidationError) -> JSONResponse:
    request_id = _extract_request_id(request)
    content = {"detail": exc.errors()}
    if request_id:
        content["request_id"] = request_id
    headers = {"X-Request-ID": request_id} if request_id else {}
    return JSONResponse(
        status_code=422,
        content=content,
        headers=headers,
    )


@app.exception_handler(RedisAdapterError)
@app.exception_handler(RedisError)
async def redis_unavailable(request: Request, exc: Exception) -> JSONResponse:
    """Keep infrastructure details in server logs and return a stable API error."""
    request_id = _extract_request_id(request)
    error(f"Redis request failure [request_id={request_id}]: {type(exc).__name__}")
    content = {"detail": "Study service is temporarily unavailable. Please try again shortly."}
    if request_id:
        content["request_id"] = request_id
    headers = {"X-Request-ID": request_id} if request_id else {}
    return JSONResponse(
        status_code=503,
        content=content,
        headers=headers,
    )

app.include_router(study_router, tags=["studies"])

app.include_router(question_router, tags=["questions"])

app.include_router(lifecycle_router, tags=["study lifecycle"])

app.include_router(waitlist_router, tags=["waitlist"])

app.include_router(quiz_router, tags=["quizzes"])

app.include_router(logs_router)
