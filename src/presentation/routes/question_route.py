from __future__ import annotations

from fastapi import APIRouter, HTTPException, Request, Response, status
from pydantic import BaseModel, Field

from src.core.settings import app_settings
from src.dal.local.study_repository import StudyRepository
from src.dal.remote.cortex_adapter import CortexAdapter
from src.domain.models.generation_policy import StudyDifficulty
from src.domain.models.study_question import D2Visual, StudyQuestion
from src.domain.services.generation_policy_service import GenerationPolicyService
from src.domain.services.question_generation_service import QuestionContractError, QuestionGenerationService
from src.presentation.routes.study_route import _fsm, _owner_id

question_router = APIRouter(prefix="/studies/{study_id}/questions")


class GenerateQuestionsPayload(BaseModel):
    difficulty: StudyDifficulty
    idempotency_key: str = Field(min_length=16, max_length=128)


def _service(request: Request) -> QuestionGenerationService:
    settings = app_settings()
    cortex = CortexAdapter(settings.CORTEX_BASE_URL, tenant_id=settings.CORTEX_TENANT_ID)
    return QuestionGenerationService(
        repository=StudyRepository(request.app.state.redis), fsm=_fsm(),
        policy=GenerationPolicyService(redis=request.app.state.redis, cortex=cortex, settings=settings),
    )


@question_router.post("/generate", status_code=status.HTTP_201_CREATED)
async def generate_questions(study_id: str, payload: GenerateQuestionsPayload, request: Request) -> dict:
    owner_id = _owner_id(request)
    repository = StudyRepository(request.app.state.redis)
    study = await repository.get_owned(owner_id=owner_id, study_id=study_id)
    if study is None:
        raise HTTPException(status_code=404, detail="Study not found")
    try:
        questions = await _service(request).generate(user_id=owner_id, study=study, difficulty=payload.difficulty, idempotency_key=payload.idempotency_key)
    except QuestionContractError as exc:
        detail = str(exc)
        if detail in {"generation_unavailable", "generation_already_running", "generation_failed"}:
            raise HTTPException(status_code=503, detail="Question generation is temporarily unavailable") from None
        if detail == "generation_quota_exhausted":
            raise HTTPException(status_code=429, detail="Question generation limit reached") from None
        raise HTTPException(status_code=422, detail="Question generation could not use this study") from None
    return {"data": [question.for_answering() for question in questions], "message": "Questions generated"}


@question_router.get("/{question_id}/visual")
async def render_question_visual(study_id: str, question_id: str, request: Request) -> Response:
    owner_id = _owner_id(request)
    repository = StudyRepository(request.app.state.redis)
    if await repository.get_owned(owner_id=owner_id, study_id=study_id) is None:
        raise HTTPException(status_code=404, detail="Study not found")
    raw = await repository.get_question(study_id=study_id, question_id=question_id)
    if raw is None:
        raise HTTPException(status_code=404, detail="Question not found")
    question = StudyQuestion.model_validate(raw)
    if not isinstance(question.visual, D2Visual):
        raise HTTPException(status_code=404, detail="Diagram not found")
    try:
        return Response(content=QuestionGenerationService.render_d2_svg(question.visual), media_type="image/svg+xml", headers={"Cache-Control": "private, max-age=300"})
    except QuestionContractError:
        raise HTTPException(status_code=503, detail="Diagram is temporarily unavailable") from None
