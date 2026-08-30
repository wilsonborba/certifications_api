from __future__ import annotations

from fastapi import APIRouter, HTTPException, Request, Response, status

from src.core.settings import app_settings
from src.dal.local.study_repository import StudyRepository
from src.dal.remote.cortex_adapter import CortexAdapter
from src.domain.services.study_completion_service import StudyCompletionError, StudyCompletionService
from src.presentation.routes.study_route import _account_used_bytes, _fsm, _owner_id, _study_response

lifecycle_router = APIRouter(prefix="/studies/{study_id}")


def _service(request: Request) -> StudyCompletionService:
    settings = app_settings()
    return StudyCompletionService(
        repository=StudyRepository(request.app.state.redis), fsm=_fsm(),
        cortex=CortexAdapter(settings.CORTEX_BASE_URL, tenant_id=settings.CORTEX_TENANT_ID),
    )


@lifecycle_router.post("/complete")
async def complete_study(study_id: str, request: Request) -> dict:
    owner_id = _owner_id(request)
    repository = StudyRepository(request.app.state.redis)
    study = await repository.get_owned(owner_id=owner_id, study_id=study_id)
    if study is None:
        raise HTTPException(status_code=404, detail="Study not found")
    try:
        completed = await _service(request).complete(study)
    except StudyCompletionError:
        raise HTTPException(status_code=503, detail="Study completion is temporarily unavailable") from None
    settings = app_settings()
    account_used_bytes = await _account_used_bytes(repository, owner_id)
    return {
        "data": _study_response(completed, account_used_bytes=account_used_bytes, account_max_bytes=settings.USER_TOTAL_MAX_BYTES),
        "message": "Study completed",
    }


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
