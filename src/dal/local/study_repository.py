from __future__ import annotations

from datetime import UTC, datetime
from uuid import uuid4

from src.dal.local.redis_adapter import RedisAdapter
from src.domain.models.study import Study, StudySource
from src.domain.models.study_question import StudyQuestion


class StudyRepository:
    """Redis-backed durable study metadata; FSM remains the binary source store."""

    def __init__(self, redis: RedisAdapter) -> None:
        self._redis = redis

    def _study_key(self, study_id: str) -> str:
        return self._redis.k("studies", study_id)

    def _owner_key(self, owner_id: str) -> str:
        return self._redis.k("studies", "owner", owner_id)

    async def create(self, *, owner_id: str, name: str) -> Study:
        now = datetime.now(UTC)
        study = Study(id=str(uuid4()), owner_id=owner_id, name=name, created_at=now, updated_at=now)
        await self.save(study)
        await self._redis.sadd(self._owner_key(owner_id), study.id)
        return study

    async def save(self, study: Study) -> Study:
        study.updated_at = datetime.now(UTC)
        await self._redis.set(self._study_key(study.id), study.model_dump(mode="json"))
        return study

    async def get_owned(self, *, owner_id: str, study_id: str) -> Study | None:
        raw = await self._redis.get(self._study_key(study_id))
        if raw is None:
            return None
        study = Study.model_validate(raw)
        return study if study.owner_id == owner_id else None

    async def list_owned(self, *, owner_id: str) -> list[Study]:
        ids = await self._redis.smembers(self._owner_key(owner_id))
        studies = [await self.get_owned(owner_id=owner_id, study_id=str(study_id)) for study_id in ids]
        return sorted((study for study in studies if study is not None), key=lambda study: study.updated_at, reverse=True)

    async def add_source(self, *, study: Study, source: StudySource) -> Study:
        study.sources.append(source)
        return await self.save(study)

    async def replace_source(self, *, study: Study, source: StudySource) -> Study:
        study.sources = [source if item.id == source.id else item for item in study.sources]
        return await self.save(study)

    async def remove_source(self, *, study: Study, source_id: str) -> StudySource | None:
        source = next((item for item in study.sources if item.id == source_id), None)
        if source is None:
            return None
        study.sources = [item for item in study.sources if item.id != source_id]
        await self.save(study)
        return source

    async def get_question(self, *, study_id: str, question_id: str) -> dict | None:
        return await self._redis.get(self._redis.k("study_questions", study_id, question_id))

    async def save_question(self, *, study_id: str, question: StudyQuestion) -> None:
        await self._redis.set(self._redis.k("study_questions", study_id, question.id), question.model_dump(mode="json"))
        await self._redis.sadd(self._redis.k("study_questions", "study", study_id), question.id)
