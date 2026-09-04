from __future__ import annotations

from fastapi import APIRouter, HTTPException, Request, Response, status
from pydantic import BaseModel, Field

from src.core.logs import error, info
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
    use_web: bool = False
    # None (or omitted) means "as many as the source material supports".
    question_count: int | None = Field(default=None, ge=1, le=20)


class AnswerSubmission(BaseModel):
    question_id: str
    choice_index: int = Field(ge=-1)


class SubmitAnswersPayload(BaseModel):
    answers: list[AnswerSubmission] = Field(min_length=1, max_length=50)


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
    info(f"generate_questions study={study_id} use_web={payload.use_web} question_count={payload.question_count} difficulty={payload.difficulty}")
    try:
        questions = await _service(request).generate(
            user_id=owner_id,
            study=study,
            difficulty=payload.difficulty,
            idempotency_key=payload.idempotency_key,
            use_web=payload.use_web,
            question_count=payload.question_count,
        )
    except QuestionContractError as exc:
        detail = str(exc)
        if detail in {"generation_unavailable", "generation_already_running", "generation_failed"}:
            raise HTTPException(status_code=503, detail="Question generation is temporarily unavailable") from None
        if detail == "generation_quota_exhausted":
            raise HTTPException(status_code=429, detail="Question generation limit reached") from None
        # Every other reason ends as a generic 422 for the client, but the
        # real cause (contract validation failure, no ready sources, etc.)
        # must not be silently discarded here - that's what made this class
        # of failure impossible to diagnose from the logs before.
        error(f"Question generation for study {study_id} returned 422: {detail}")
        raise HTTPException(status_code=422, detail="Question generation could not use this study") from None
    return {"data": [question.for_answering() for question in questions], "message": "Questions generated"}


@question_router.get("/progress")
async def get_generation_progress(study_id: str, request: Request) -> dict:
    """Polled by the frontend while /generate is in flight to show real
    "N questions generated so far" progress instead of a static spinner."""
    owner_id = _owner_id(request)
    repository = StudyRepository(request.app.state.redis)
    if await repository.get_owned(owner_id=owner_id, study_id=study_id) is None:
        raise HTTPException(status_code=404, detail="Study not found")
    progress = await repository.get_generation_progress(study_id=study_id)
    return {
        "data": progress
        or {
            "status": "idle",
            "chunks_done": 0,
            "chunks_total": 0,
            "questions_generated": 0,
            "questions_target": None,
        },
    }


@question_router.post("/submit")
async def submit_answers(study_id: str, payload: SubmitAnswersPayload, request: Request) -> dict:
    owner_id = _owner_id(request)
    repository = StudyRepository(request.app.state.redis)
    study = await repository.get_owned(owner_id=owner_id, study_id=study_id)
    if study is None:
        raise HTTPException(status_code=404, detail="Study not found")

    results: list[dict] = []
    correct_count = 0
    for answer in payload.answers:
        raw = await repository.get_question(study_id=study_id, question_id=answer.question_id)
        if raw is None:
            raise HTTPException(status_code=404, detail="Question not found")
        question = StudyQuestion.model_validate(raw)
        is_correct = answer.choice_index == question.correct_index
        if is_correct:
            correct_count += 1
        results.append(
            {
                "question_id": question.id,
                "chosen_index": answer.choice_index,
                "correct_index": question.correct_index,
                "is_correct": is_correct,
                "explanation": question.explanation,
            }
        )

    total = len(payload.answers)
    wrong_count = total - correct_count
    score = round((correct_count / total) * 100, 2) if total else 0.0
    return {
        "data": {
            "score": score,
            "correct_count": correct_count,
            "wrong_count": wrong_count,
            "total_questions": total,
            "results": results,
        },
        "message": "Answers graded",
    }


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
