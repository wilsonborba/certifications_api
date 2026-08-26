from __future__ import annotations

import hashlib
from datetime import UTC, datetime
from uuid import uuid4

from fastapi import APIRouter, File, HTTPException, Request, UploadFile, status
from pydantic import BaseModel, Field

from src.core.logs import error
from src.core.settings import app_settings
from src.dal.local.study_repository import StudyRepository
from src.dal.remote.fsm_s3_adapter import FsmConfigurationError, FsmS3Adapter, FsmStorageError
from src.domain.models.study import SourceKind, SourceSelection, SourceStatus, Study, StudySource, StudyStatus
from src.domain.services.study_ingestion_service import IngestionError, StudyIngestionService
from src.dal.remote.cortex_adapter import CortexAdapter

study_router = APIRouter(prefix="/studies")


class CreateStudyPayload(BaseModel):
    name: str = Field(min_length=1, max_length=120)


class SelectionPayload(BaseModel):
    selection: SourceSelection


def _owner_id(request: Request) -> str:
    value = request.headers.get("x-uuid")
    if not value:
        raise HTTPException(status_code=status.HTTP_401_UNAUTHORIZED, detail="Authentication is required")
    return value


def _repo(request: Request) -> StudyRepository:
    return StudyRepository(request.app.state.redis)


def _fsm() -> FsmS3Adapter:
    settings = app_settings()
    return FsmS3Adapter(
        endpoint=settings.FSM_S3_ENDPOINT,
        access_key=settings.FSM_S3_ACCESS_KEY,
        secret_key=settings.FSM_S3_SECRET_KEY,
        region=settings.FSM_S3_REGION,
        bucket=settings.FSM_S3_BUCKET,
    )


def _study_response(study: Study) -> dict:
    return study.model_dump(mode="json") | {"active_size_bytes": study.active_size_bytes, "retained_size_bytes": study.retained_size_bytes}


def _ingestion_service() -> StudyIngestionService:
    settings = app_settings()
    return StudyIngestionService(
        fsm=_fsm(),
        cortex=CortexAdapter(
            settings.CORTEX_BASE_URL,
            timeout_seconds=settings.CORTEX_TIMEOUT_SECONDS,
            tenant_id=settings.CORTEX_TENANT_ID,
        ),
        settings=settings,
    )


@study_router.post("", status_code=status.HTTP_201_CREATED)
async def create_study(payload: CreateStudyPayload, request: Request) -> dict:
    study = await _repo(request).create(owner_id=_owner_id(request), name=payload.name.strip())
    return {"data": _study_response(study), "message": "Study created"}


@study_router.get("")
async def list_studies(request: Request) -> dict:
    studies = await _repo(request).list_owned(owner_id=_owner_id(request))
    return {"data": [_study_response(study) for study in studies], "message": "Studies retrieved"}


@study_router.get("/{study_id}")
async def get_study(study_id: str, request: Request) -> dict:
    study = await _repo(request).get_owned(owner_id=_owner_id(request), study_id=study_id)
    if study is None:
        raise HTTPException(status_code=404, detail="Study not found")
    return {"data": _study_response(study), "message": "Study retrieved"}


@study_router.post("/{study_id}/sources", status_code=status.HTTP_201_CREATED)
async def upload_source(
    study_id: str,
    request: Request,
    kind: SourceKind,
    file: UploadFile = File(...),
) -> dict:
    owner_id = _owner_id(request)
    repository = _repo(request)
    study = await repository.get_owned(owner_id=owner_id, study_id=study_id)
    if study is None:
        raise HTTPException(status_code=404, detail="Study not found")
    if study.status in {StudyStatus.completed, StudyStatus.deleting, StudyStatus.deleted}:
        raise HTTPException(status_code=409, detail="Study can no longer accept sources")
    raw = await file.read()
    settings = app_settings()
    size = len(raw)
    if not raw:
        raise HTTPException(status_code=422, detail="A non-empty file is required")
    if size > settings.STUDY_SOURCE_MAX_BYTES:
        raise HTTPException(status_code=413, detail="Source exceeds the per-file limit")
    if study.active_size_bytes + size > settings.STUDY_ACTIVE_MAX_BYTES:
        raise HTTPException(status_code=413, detail="Study storage limit exceeded")
    if kind is SourceKind.pdf and file.content_type not in {"application/pdf", "application/x-pdf"}:
        raise HTTPException(status_code=422, detail="A PDF file is required")
    source_id = str(uuid4())
    filename = file.filename or f"{source_id}.{kind.value}"
    object_key = _fsm().object_key(study_id=study.id, source_id=source_id, filename=filename)
    content_type = file.content_type or "application/octet-stream"
    try:
        await _fsm().put(key=object_key, body=raw, content_type=content_type)
    except FsmConfigurationError:
        raise HTTPException(status_code=503, detail="Storage is temporarily unavailable") from None
    except FsmStorageError as exc:
        error(f"FSM source upload failed for study {study.id}: {exc}")
        raise HTTPException(status_code=503, detail="Storage is temporarily unavailable") from None
    now = datetime.now(UTC)
    source = StudySource(
        id=source_id, study_id=study.id, owner_id=owner_id, kind=kind,
        filename=filename, content_type=content_type, size_bytes=size,
        sha256=hashlib.sha256(raw).hexdigest(), object_key=object_key,
        status=SourceStatus.uploaded, created_at=now, updated_at=now,
    )
    study.status = StudyStatus.collecting
    await repository.add_source(study=study, source=source)
    return {"data": source.model_dump(mode="json"), "message": "Source uploaded"}


@study_router.patch("/{study_id}/sources/{source_id}/selection")
async def update_source_selection(study_id: str, source_id: str, payload: SelectionPayload, request: Request) -> dict:
    owner_id = _owner_id(request)
    repository = _repo(request)
    study = await repository.get_owned(owner_id=owner_id, study_id=study_id)
    if study is None:
        raise HTTPException(status_code=404, detail="Study not found")
    source = next((item for item in study.sources if item.id == source_id), None)
    if source is None:
        raise HTTPException(status_code=404, detail="Source not found")
    try:
        payload.selection.validate_for(source.kind)
    except ValueError as exc:
        raise HTTPException(status_code=422, detail=str(exc)) from None
    source.selection = payload.selection
    source.updated_at = datetime.now(UTC)
    await repository.replace_source(study=study, source=source)
    return {"data": source.model_dump(mode="json"), "message": "Source selection updated"}


@study_router.post("/{study_id}/sources/{source_id}/ingest")
async def ingest_source(study_id: str, source_id: str, request: Request) -> dict:
    owner_id = _owner_id(request)
    repository = _repo(request)
    study = await repository.get_owned(owner_id=owner_id, study_id=study_id)
    if study is None:
        raise HTTPException(status_code=404, detail="Study not found")
    source = next((item for item in study.sources if item.id == source_id), None)
    if source is None:
        raise HTTPException(status_code=404, detail="Source not found")
    if source.status is SourceStatus.processing:
        raise HTTPException(status_code=409, detail="Source processing is already in progress")
    source.status = SourceStatus.processing
    await repository.replace_source(study=study, source=source)
    try:
        artifact = await _ingestion_service().ingest(source)
        body = artifact.as_bytes()
        settings = app_settings()
        if study.active_size_bytes + len(body) > settings.STUDY_ACTIVE_MAX_BYTES:
            raise IngestionError("Processing would exceed the study storage limit")
        source.derived_object_key = f"studies/{study.id}/derived/{source.id}/ingestion.json"
        await _fsm().put(key=source.derived_object_key, body=body, content_type="application/json")
        source.derived_size_bytes = len(body)
        source.status = SourceStatus.ready
        source.updated_at = datetime.now(UTC)
        study.status = StudyStatus.ready if all(item.status is SourceStatus.ready for item in study.sources) else StudyStatus.processing
        await repository.replace_source(study=study, source=source)
        return {"data": source.model_dump(mode="json"), "message": "Source processed"}
    except (IngestionError, FsmConfigurationError, FsmStorageError) as exc:
        error(f"Study source ingestion failed for {study.id}/{source.id}: {exc}")
        source.status = SourceStatus.failed
        source.updated_at = datetime.now(UTC)
        await repository.replace_source(study=study, source=source)
        raise HTTPException(status_code=503, detail="Source processing is temporarily unavailable") from None


@study_router.delete("/{study_id}/sources/{source_id}", status_code=status.HTTP_204_NO_CONTENT)
async def remove_source(study_id: str, source_id: str, request: Request) -> None:
    owner_id = _owner_id(request)
    repository = _repo(request)
    study = await repository.get_owned(owner_id=owner_id, study_id=study_id)
    if study is None:
        raise HTTPException(status_code=404, detail="Study not found")
    source = next((item for item in study.sources if item.id == source_id), None)
    if source is None:
        raise HTTPException(status_code=404, detail="Source not found")
    try:
        await _fsm().delete(key=source.object_key)
        if source.derived_object_key:
            await _fsm().delete(key=source.derived_object_key)
    except (FsmConfigurationError, FsmStorageError) as exc:
        error(f"FSM source deletion failed for study {study.id}: {exc}")
        raise HTTPException(status_code=503, detail="Storage is temporarily unavailable") from None
    await repository.remove_source(study=study, source_id=source_id)
