from __future__ import annotations

import asyncio
import unittest
import uuid
from contextlib import contextmanager
from datetime import UTC, datetime, timedelta
from types import SimpleNamespace
from unittest.mock import patch

from fastapi import HTTPException

from src.dal.local.orm import CompletedQuiz, QuizShare
from src.presentation.routes.quiz_governance_route import (
    GradeSharedQuizPayload,
    SharedAnswerSubmission,
    get_completed_quiz,
    get_shared_quiz,
    grade_shared_quiz,
    render_shared_question_visual,
)

QUESTIONS = [
    {
        "id": "q1",
        "prompt": "2 + 2 = ?",
        "choices": ["3", "4", "5"],
        "visual": {"kind": "none"},
        "correct_index": 1,
        "explanation": "Basic arithmetic.",
    },
    {
        "id": "q2",
        "prompt": "The sky is what color?",
        "choices": ["Blue", "Green"],
        "visual": {"kind": "none"},
        "correct_index": 0,
        "explanation": "Rayleigh scattering.",
    },
]


class _FakeScalars:
    def __init__(self, items: list) -> None:
        self._items = items

    def first(self):
        return self._items[0] if self._items else None

    def all(self) -> list:
        return self._items


class _FakeSession:
    """Records plain Python model instances in memory; never touches a real
    database. Supports exactly the query shapes quiz_governance_route uses:
    add/flush/get plus `select(QuizShare).where(QuizShare.token == token)`."""

    def __init__(self) -> None:
        self.shares: list[QuizShare] = []
        self.quizzes: dict[str, CompletedQuiz] = {}

    def add(self, obj) -> None:
        if isinstance(obj, QuizShare):
            if obj.id is None:
                obj.id = str(uuid.uuid4())
            self.shares.append(obj)
        elif isinstance(obj, CompletedQuiz):
            if obj.id is None:
                obj.id = str(uuid.uuid4())
            self.quizzes[obj.id] = obj

    def flush(self) -> None:
        pass

    def get(self, model, pk):
        if model is CompletedQuiz:
            return self.quizzes.get(pk)
        raise AssertionError(f"unexpected model in fake session.get: {model!r}")

    def scalars(self, stmt) -> _FakeScalars:
        token = stmt.whereclause.right.value
        return _FakeScalars([s for s in self.shares if s.token == token])


class _FakeDBAdapter:
    def __init__(self, session: _FakeSession) -> None:
        self.session = session

    @contextmanager
    def session_scope(self):
        yield self.session


def _seed(*, expires_in_hours: int = 24, max_uses: int | None = None, current_uses: int = 0):
    now = datetime.now(UTC)
    session = _FakeSession()
    quiz = CompletedQuiz(
        owner_id="owner-1",
        title="Sample quiz",
        visibility="public",
        status="active",
        total_questions=len(QUESTIONS),
        total_attempts=0,
        third_party_attempts=0,
        quiz_data={"questions": QUESTIONS},
        created_at=now,
        updated_at=now,
    )
    session.add(quiz)
    share = QuizShare(
        quiz_id=quiz.id,
        token="tok123",
        expires_at=now + timedelta(hours=expires_in_hours),
        max_uses=max_uses,
        current_uses=current_uses,
        is_active=True,
        created_at=now,
    )
    session.add(share)
    return _FakeDBAdapter(session), quiz.id


class SharedQuizGradingTests(unittest.TestCase):
    def test_grades_shared_quiz_answers_without_consuming_a_use(self) -> None:
        db, _ = _seed()
        payload = GradeSharedQuizPayload(
            answers=[
                SharedAnswerSubmission(question_id="q1", choice_index=1),
                SharedAnswerSubmission(question_id="q2", choice_index=1),
            ]
        )
        with self._patch(db):
            result = asyncio.run(grade_shared_quiz("tok123", payload, SimpleNamespace()))

        data = result["data"]
        self.assertEqual(data["correct_count"], 1)
        self.assertEqual(data["wrong_count"], 1)
        self.assertEqual(data["score"], 50.0)
        by_id = {row["question_id"]: row for row in data["results"]}
        self.assertTrue(by_id["q1"]["is_correct"])
        self.assertFalse(by_id["q2"]["is_correct"])
        self.assertEqual(by_id["q2"]["correct_index"], 0)

        # Grading is a dry run: it must not consume the share's usage budget.
        self.assertEqual(db.session.shares[0].current_uses, 0)

    def test_rejects_grading_against_an_expired_share(self) -> None:
        db, _ = _seed(expires_in_hours=-1)
        payload = GradeSharedQuizPayload(answers=[SharedAnswerSubmission(question_id="q1", choice_index=1)])
        with self._patch(db):
            with self.assertRaises(HTTPException) as raised:
                asyncio.run(grade_shared_quiz("tok123", payload, SimpleNamespace()))
        self.assertEqual(raised.exception.status_code, 410)

    def test_rejects_grading_against_an_exhausted_share(self) -> None:
        db, _ = _seed(max_uses=1, current_uses=1)
        payload = GradeSharedQuizPayload(answers=[SharedAnswerSubmission(question_id="q1", choice_index=1)])
        with self._patch(db):
            with self.assertRaises(HTTPException) as raised:
                asyncio.run(grade_shared_quiz("tok123", payload, SimpleNamespace()))
        self.assertEqual(raised.exception.status_code, 410)

    def test_shared_preview_and_owner_read_never_expose_the_answer_key(self) -> None:
        db, quiz_id = _seed()
        with self._patch(db):
            shared = asyncio.run(get_shared_quiz("tok123"))
            owner_request = SimpleNamespace(headers={"x-uuid": "owner-1"})
            owned = asyncio.run(get_completed_quiz(quiz_id, owner_request))

        for payload in (shared["data"], owned["data"]):
            for question in payload["quiz_data"]["questions"]:
                self.assertNotIn("correct_index", question)
                self.assertNotIn("explanation", question)
                self.assertIn("prompt", question)
                self.assertIn("choices", question)

    def test_rejects_visual_lookup_for_a_non_diagram_question(self) -> None:
        db, _ = _seed()
        with self._patch(db):
            with self.assertRaises(HTTPException) as raised:
                asyncio.run(render_shared_question_visual("tok123", "q1"))
        self.assertEqual(raised.exception.status_code, 404)

    def _patch(self, db):
        return patch("src.presentation.routes.quiz_governance_route.DBAdapter", return_value=db)


if __name__ == "__main__":
    unittest.main()
