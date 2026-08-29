from __future__ import annotations

import secrets
from datetime import UTC, datetime, timedelta
from typing import Any

from fastapi import APIRouter, HTTPException, Request, Response, status
from pydantic import BaseModel, Field
from sqlalchemy import select
from sqlalchemy.orm import Session

from src.core.logs import error
from src.dal.local.db_adapter import DBAdapter
from src.dal.local.orm import CompletedQuiz, QuizAttempt, QuizShare, User
from src.presentation.routes.study_route import _owner_id

quiz_router = APIRouter(prefix="/quizzes")


class CreateCompletedQuizPayload(BaseModel):
    title: str = Field(min_length=1, max_length=255)
    description: str | None = None
    visibility: str = Field(default="private", pattern="^(private|public)$")
    total_questions: int = Field(ge=1)
    quiz_data: dict[str, Any]


class UpdateVisibilityPayload(BaseModel):
    visibility: str = Field(pattern="^(private|public)$")


class CreateShareTokenPayload(BaseModel):
    expires_in_hours: int = Field(default=24, ge=1, le=8760)
    max_uses: int | None = Field(default=None, ge=1)


class SubmitAttemptPayload(BaseModel):
    user_name: str | None = Field(default="Anonymous", max_length=255)
    score: float = Field(ge=0.0, le=100.0)
    correct_count: int = Field(ge=0)
    wrong_count: int = Field(ge=0)
    time_spent_seconds: int = Field(ge=0)
    answers_json: dict[str, Any] | None = None


@quiz_router.post("/completed", status_code=status.HTTP_201_CREATED)
async def save_completed_quiz(payload: CreateCompletedQuizPayload, request: Request) -> dict:
    owner_id = _owner_id(request)
    db = DBAdapter()
    now = datetime.now(UTC)

    with db.session_scope() as session:
        # Ensure owner exists in users table (Lazy Provisioning)
        user = session.get(User, owner_id)
        if not user:
            user = User(id=owner_id, email=f"{owner_id}@user.asodya.internal", created_at=now, updated_at=now)
            session.add(user)
            session.flush()

        quiz = CompletedQuiz(
            owner_id=owner_id,
            title=payload.title,
            description=payload.description,
            visibility=payload.visibility,
            status="active",
            total_questions=payload.total_questions,
            total_attempts=0,
            third_party_attempts=0,
            quiz_data=payload.quiz_data,
            created_at=now,
            updated_at=now,
        )
        session.add(quiz)
        session.flush()
        return {
            "data": {
                "id": quiz.id,
                "title": quiz.title,
                "visibility": quiz.visibility,
                "created_at": quiz.created_at.isoformat(),
            },
            "message": "Quiz saved successfully",
        }


@quiz_router.get("/completed")
async def list_completed_quizzes(request: Request) -> dict:
    owner_id = _owner_id(request)
    db = DBAdapter()
    with db.session_scope() as session:
        quizzes = session.scalars(
            select(CompletedQuiz)
            .where(CompletedQuiz.owner_id == owner_id, CompletedQuiz.status == "active")
            .order_by(CompletedQuiz.created_at.desc())
        ).all()
        return {
            "data": [
                {
                    "id": q.id,
                    "title": q.title,
                    "description": q.description,
                    "visibility": q.visibility,
                    "total_questions": q.total_questions,
                    "total_attempts": q.total_attempts,
                    "third_party_attempts": q.third_party_attempts,
                    "created_at": q.created_at.isoformat(),
                }
                for q in quizzes
            ]
        }


@quiz_router.get("/completed/{quiz_id}")
async def get_completed_quiz(quiz_id: str, request: Request) -> dict:
    owner_id = _owner_id(request)
    db = DBAdapter()
    with db.session_scope() as session:
        quiz = session.get(CompletedQuiz, quiz_id)
        if not quiz or quiz.status != "active":
            raise HTTPException(status_code=404, detail="Quiz not found")
        if quiz.visibility == "private" and quiz.owner_id != owner_id:
            raise HTTPException(status_code=403, detail="Access denied to private quiz")
        return {
            "data": {
                "id": quiz.id,
                "owner_id": quiz.owner_id,
                "title": quiz.title,
                "description": quiz.description,
                "visibility": quiz.visibility,
                "total_questions": quiz.total_questions,
                "total_attempts": quiz.total_attempts,
                "third_party_attempts": quiz.third_party_attempts,
                "quiz_data": quiz.quiz_data,
                "created_at": quiz.created_at.isoformat(),
            }
        }


@quiz_router.patch("/completed/{quiz_id}/visibility")
async def update_quiz_visibility(
    quiz_id: str, payload: UpdateVisibilityPayload, request: Request
) -> dict:
    owner_id = _owner_id(request)
    db = DBAdapter()
    with db.session_scope() as session:
        quiz = session.get(CompletedQuiz, quiz_id)
        if not quiz or quiz.owner_id != owner_id:
            raise HTTPException(status_code=404, detail="Quiz not found")
        quiz.visibility = payload.visibility
        quiz.updated_at = datetime.now(UTC)
        return {"data": {"id": quiz.id, "visibility": quiz.visibility}, "message": "Visibility updated"}


@quiz_router.delete("/completed/{quiz_id}", status_code=status.HTTP_204_NO_CONTENT)
async def delete_completed_quiz(quiz_id: str, request: Request) -> Response:
    owner_id = _owner_id(request)
    db = DBAdapter()
    with db.session_scope() as session:
        quiz = session.get(CompletedQuiz, quiz_id)
        if not quiz or quiz.owner_id != owner_id:
            raise HTTPException(status_code=404, detail="Quiz not found")

        # Deletion Governance Rule: Block deletion of public quizzes with active third-party attempts
        if quiz.visibility == "public" and quiz.third_party_attempts > 0:
            raise HTTPException(
                status_code=status.HTTP_403_FORBIDDEN,
                detail="Public quizzes with active participant attempts cannot be deleted to preserve leaderboard history.",
            )

        session.delete(quiz)
        return Response(status_code=status.HTTP_204_NO_CONTENT)


@quiz_router.post("/completed/{quiz_id}/share", status_code=status.HTTP_201_CREATED)
async def create_share_token(
    quiz_id: str, payload: CreateShareTokenPayload, request: Request
) -> dict:
    owner_id = _owner_id(request)
    db = DBAdapter()
    now = datetime.now(UTC)
    expires_at = now + timedelta(hours=payload.expires_in_hours)
    token = secrets.token_urlsafe(32)

    with db.session_scope() as session:
        quiz = session.get(CompletedQuiz, quiz_id)
        if not quiz or quiz.owner_id != owner_id:
            raise HTTPException(status_code=404, detail="Quiz not found")

        share = QuizShare(
            quiz_id=quiz.id,
            token=token,
            expires_at=expires_at,
            max_uses=payload.max_uses,
            current_uses=0,
            is_active=True,
            created_at=now,
        )
        session.add(share)
        session.flush()
        return {
            "data": {
                "token": share.token,
                "expires_at": share.expires_at.isoformat(),
                "max_uses": share.max_uses,
            },
            "message": "Share link generated",
        }


@quiz_router.get("/shared/{token}")
async def get_shared_quiz(token: str) -> dict:
    db = DBAdapter()
    now = datetime.now(UTC)
    with db.session_scope() as session:
        share = session.scalars(select(QuizShare).where(QuizShare.token == token)).first()
        if not share or not share.is_active or share.expires_at < now:
            raise HTTPException(status_code=410, detail="Shared link has expired or is invalid")
        if share.max_uses is not None and share.current_uses >= share.max_uses:
            raise HTTPException(status_code=410, detail="Shared link usage limit reached")

        quiz = session.get(CompletedQuiz, share.quiz_id)
        if not quiz or quiz.status != "active":
            raise HTTPException(status_code=404, detail="Quiz not found")

        return {
            "data": {
                "quiz_id": quiz.id,
                "title": quiz.title,
                "description": quiz.description,
                "total_questions": quiz.total_questions,
                "quiz_data": quiz.quiz_data,
                "expires_at": share.expires_at.isoformat(),
            }
        }


@quiz_router.post("/shared/{token}/attempt", status_code=status.HTTP_201_CREATED)
async def submit_quiz_attempt(
    token: str, payload: SubmitAttemptPayload, request: Request
) -> dict:
    db = DBAdapter()
    now = datetime.now(UTC)
    caller_uuid = request.headers.get("x-uuid")

    with db.session_scope() as session:
        share = session.scalars(select(QuizShare).where(QuizShare.token == token)).first()
        if not share or not share.is_active or share.expires_at < now:
            raise HTTPException(status_code=410, detail="Shared link has expired or is invalid")

        quiz = session.get(CompletedQuiz, share.quiz_id)
        if not quiz or quiz.status != "active":
            raise HTTPException(status_code=404, detail="Quiz not found")

        # Increment usage & attempts
        share.current_uses += 1
        quiz.total_attempts += 1
        if caller_uuid != quiz.owner_id:
            quiz.third_party_attempts += 1

        attempt = QuizAttempt(
            quiz_id=quiz.id,
            user_id=caller_uuid if caller_uuid else None,
            user_name=payload.user_name,
            score=payload.score,
            correct_count=payload.correct_count,
            wrong_count=payload.wrong_count,
            time_spent_seconds=payload.time_spent_seconds,
            answers_json=payload.answers_json,
            completed_at=now,
        )
        session.add(attempt)
        session.flush()

        return {
            "data": {
                "attempt_id": attempt.id,
                "score": attempt.score,
                "completed_at": attempt.completed_at.isoformat(),
            },
            "message": "Attempt recorded",
        }


@quiz_router.get("/completed/{quiz_id}/leaderboard")
async def get_quiz_leaderboard(quiz_id: str, request: Request) -> dict:
    db = DBAdapter()
    caller_uuid = request.headers.get("x-uuid")

    with db.session_scope() as session:
        quiz = session.get(CompletedQuiz, quiz_id)
        if not quiz or quiz.status != "active":
            raise HTTPException(status_code=404, detail="Quiz not found")

        if quiz.visibility == "private" and quiz.owner_id != caller_uuid:
            raise HTTPException(status_code=403, detail="Leaderboard unavailable for private quiz")

        attempts = session.scalars(
            select(QuizAttempt)
            .where(QuizAttempt.quiz_id == quiz_id)
            .order_by(
                QuizAttempt.score.desc(),
                QuizAttempt.time_spent_seconds.asc(),
                QuizAttempt.completed_at.asc(),
            )
            .limit(100)
        ).all()

        return {
            "data": [
                {
                    "rank": index + 1,
                    "user_name": a.user_name or "Anonymous",
                    "score": float(a.score),
                    "correct_count": a.correct_count,
                    "wrong_count": a.wrong_count,
                    "time_spent_seconds": a.time_spent_seconds,
                    "completed_at": a.completed_at.isoformat(),
                }
                for index, a in enumerate(attempts)
            ]
        }
