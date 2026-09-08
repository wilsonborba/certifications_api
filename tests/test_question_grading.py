from __future__ import annotations

import asyncio
import unittest
from types import SimpleNamespace

from fastapi import HTTPException

from src.presentation.routes.question_route import (
    AnswerSubmission,
    SubmitAnswersPayload,
    submit_answers,
)


class _Redis:
    """In-memory stand-in for RedisAdapter; never touches a real Redis."""

    def __init__(self) -> None:
        self.values: dict[str, object] = {}
        self.sets: dict[str, set] = {}

    def k(self, *parts: object) -> str:
        return ":".join(str(part) for part in parts)

    async def get(self, key: str):
        return self.values.get(key)

    async def set(self, key: str, value: object, **kwargs) -> bool:
        self.values[key] = value
        return True

    async def sadd(self, key: str, *members: object) -> None:
        self.sets.setdefault(key, set()).update(str(m) for m in members)

    async def smembers(self, key: str):
        return self.sets.get(key, set())

    async def delete(self, *keys: str) -> None:
        for key in keys:
            self.values.pop(key, None)
            self.sets.pop(key, None)


def _request(owner_id: str = "owner-1") -> SimpleNamespace:
    return SimpleNamespace(
        headers={"x-uuid": owner_id},
        app=SimpleNamespace(state=SimpleNamespace(redis=_Redis())),
    )


def _seed_study(request, study_id: str, owner_id: str) -> None:
    redis = request.app.state.redis
    asyncio.run(
        redis.set(
            redis.k("studies", study_id),
            {
                "id": study_id,
                "owner_id": owner_id,
                "name": "Sample study",
                "status": "draft",
                "active_size_bytes": 0,
                "sources": [],
                "created_at": "2026-01-01T00:00:00+00:00",
                "updated_at": "2026-01-01T00:00:00+00:00",
            },
        )
    )


def _seed_question(request, study_id: str, question_id: str, correct_index: int) -> None:
    redis = request.app.state.redis
    asyncio.run(
        redis.set(
            redis.k("study_questions", study_id, question_id),
            {
                "id": question_id,
                "prompt": "2 + 2 = ?",
                "choices": ["3", "4", "5", "6"],
                "correct_index": correct_index,
                "explanation": "Basic arithmetic.",
                "citations": [{"source": "textbook"}],
                "visual": {"kind": "none"},
                "tier_requested": 0,
                "schema_version": 1,
            },
        )
    )


class SubmitAnswersTests(unittest.TestCase):
    def test_grades_correct_and_incorrect_answers_without_leaking_the_key_upfront(self) -> None:
        request = _request()
        _seed_study(request, "study-1", owner_id="owner-1")
        _seed_question(request, "study-1", "q1", correct_index=1)
        _seed_question(request, "study-1", "q2", correct_index=0)

        payload = SubmitAnswersPayload(
            answers=[
                AnswerSubmission(question_id="q1", choice_index=1),
                AnswerSubmission(question_id="q2", choice_index=2),
            ]
        )

        result = asyncio.run(submit_answers("study-1", payload, request))

        data = result["data"]
        self.assertEqual(data["correct_count"], 1)
        self.assertEqual(data["wrong_count"], 1)
        self.assertEqual(data["total_questions"], 2)
        self.assertEqual(data["score"], 50.0)
        by_id = {row["question_id"]: row for row in data["results"]}
        self.assertTrue(by_id["q1"]["is_correct"])
        self.assertEqual(by_id["q1"]["correct_index"], 1)
        self.assertFalse(by_id["q2"]["is_correct"])
        self.assertEqual(by_id["q2"]["correct_index"], 0)

    def test_rejects_grading_a_study_owned_by_someone_else(self) -> None:
        request = _request(owner_id="owner-1")
        _seed_study(request, "study-1", owner_id="someone-else")
        _seed_question(request, "study-1", "q1", correct_index=0)

        payload = SubmitAnswersPayload(answers=[AnswerSubmission(question_id="q1", choice_index=0)])

        with self.assertRaises(HTTPException) as raised:
            asyncio.run(submit_answers("study-1", payload, request))
        self.assertEqual(raised.exception.status_code, 404)

    def test_rejects_an_answer_for_a_question_that_does_not_exist(self) -> None:
        request = _request()
        _seed_study(request, "study-1", owner_id="owner-1")

        payload = SubmitAnswersPayload(answers=[AnswerSubmission(question_id="missing", choice_index=0)])

        with self.assertRaises(HTTPException) as raised:
            asyncio.run(submit_answers("study-1", payload, request))
        self.assertEqual(raised.exception.status_code, 404)


if __name__ == "__main__":
    unittest.main()
