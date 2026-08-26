from __future__ import annotations

from fastapi import APIRouter, HTTPException, Request, Response, status

from src.core.settings import app_settings
from src.dal.local.study_repository import StudyRepository
from src.dal.remote.cortex_adapter import CortexAdapter
from src.domain.services.study_completion_service import StudyCompletionError, StudyCompletionService
from src.presentation.routes.study_route import _fsm, _owner_id, _study_response

lifecycle_router = APIRouter(prefix="/studies/{study_id}")


def _service(request: Request) -> StudyCompletionService:
    settings = app_settings()
    return StudyCompletionService(
        repository=StudyRepository(request.app.state.redis), fsm=_fsm(),
        cortex=CortexAdapter(settings.CORTEX_BASE_URL, timeout_seconds=settings.CORTEX_TIMEOUT_SECONDS, tenant_id=settings.CORTEX_TENANT_ID),
    )


@lifecycle_router.post("/complete")
async def complete_study(study_id: str, request: Request) -> dict:
    repository = StudyRepository(request.app.state.redis)
    study = await repository.get_owned(owner_id=_owner_id(request), study_id=study_id)
    if study is None:
        raise HTTPException(status_code=404, detail="Study not found")
    try:
        completed = await _service(request).complete(study)
    except StudyCompletionError:
        raise HTTPException(status_code=503, detail="Study completion is temporarily unavailable") from None
    return {"data": _study_response(completed), "message": "Study completed"}


@lifecycle_router.get("/memory")
async def get_study_memory(study_id: str, request: Request) -> dict:
    repository = StudyRepository(request.app.state.redis)
    study = await repository.get_owned(owner_id=_owner_id(request), study_id=study_id)
    if study is None:
        raise HTTPException(status_code=404, detail="Study not found")
    try:
        return {"data": await _service(request).memory(study), "message": "Study memory retrieved"}
    except StudyCompletionError:
        raise HTTPException(status_code=404, detail="Study memory is unavailable") from None


@lifecycle_router.delete("", status_code=status.HTTP_204_NO_CONTENT)
async def delete_study(study_id: str, request: Request) -> Response:
    repository = StudyRepository(request.app.state.redis)
    study = await repository.get_owned(owner_id=_owner_id(request), study_id=study_id)
    if study is None:
        raise HTTPException(status_code=404, detail="Study not found")
    try:
        await _service(request).delete(study)
    except StudyCompletionError:
        raise HTTPException(status_code=503, detail="Study deletion is temporarily unavailable") from None
    return Response(status_code=status.HTTP_204_NO_CONTENT)
