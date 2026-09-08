from __future__ import annotations

import hashlib
import json
from datetime import UTC, datetime

from src.dal.local.study_repository import StudyRepository
from src.dal.remote.cortex_adapter import CortexAdapter, CortexUnavailableError
from src.dal.remote.fsm_media_adapter import FsmConfigurationError, FsmMediaAdapter, FsmStorageError
from src.domain.models.study import SourceStatus, Study, StudyStatus


class StudyCompletionError(RuntimeError):
    pass


class StudyCompletionService:
    max_memory_bytes = 10 * 1024 * 1024

    def __init__(self, *, repository: StudyRepository, fsm: FsmMediaAdapter, cortex: CortexAdapter) -> None:
        self._repository = repository
        self._fsm = fsm
        self._cortex = cortex

    async def complete(self, study: Study) -> Study:
        if study.status is StudyStatus.completed:
            return study
        ready_sources = [source for source in study.sources if source.status is SourceStatus.ready and source.derived_object_key]
        if not ready_sources:
            raise StudyCompletionError("Study has no completed source context")
        try:
            artifacts = [json.loads((await self._fsm.get(key=source.derived_object_key or "")).decode()) for source in ready_sources]
        except (ValueError, UnicodeDecodeError, FsmConfigurationError, FsmStorageError) as exc:
            raise StudyCompletionError("Study context is unavailable") from exc
        context = "\n\n".join(str(item.get("text", "")) for item in artifacts)[:100_000]
        try:
            result = await self._cortex.execute_question_generation(
                tier=0,
                prompt="Summarize this study context for a future learner. Preserve key concepts and source-linked facts; do not invent information.\n\n" + context,
            )
        except CortexUnavailableError as exc:
            raise StudyCompletionError("Study compaction is temporarily unavailable") from exc
        if not result.success or not result.response.strip():
            raise StudyCompletionError("Study compaction failed")
        memory = {
            "version": 1,
            "study_id": study.id,
            "completed_at": datetime.now(UTC).isoformat(),
            "summary": result.response.strip(),
            "sources": [
                {
                    "source_id": source.id,
                    "filename": source.filename,
                    "kind": source.kind.value,
                    "selection": source.selection.model_dump(mode="json") if source.selection else None,
                    "sha256": source.sha256,
                    "derived_sha256": hashlib.sha256(json.dumps(artifact, sort_keys=True).encode()).hexdigest(),
                }
                for source, artifact in zip(ready_sources, artifacts, strict=True)
            ],
        }
        body = json.dumps(memory, ensure_ascii=False, separators=(",", ":")).encode()
        if len(body) > self.max_memory_bytes:
            raise StudyCompletionError("Study memory exceeds the completed-study limit")
        try:
            memory_key = await self._fsm.upload(
                album=FsmMediaAdapter.album(study.id),
                filename="study-memory.json",
                body=body,
                content_type="application/json",
            )
        except (FsmConfigurationError, FsmStorageError) as exc:
            raise StudyCompletionError("Study memory could not be stored") from exc
        # The compact package is now durable. Only then is removal safe.
        try:
            for source in study.sources:
                await self._fsm.delete(key=source.object_key)
                if source.derived_object_key:
                    await self._fsm.delete(key=source.derived_object_key)
                source.status = SourceStatus.deleted
                source.derived_size_bytes = 0
                source.derived_object_key = None
        except (FsmConfigurationError, FsmStorageError) as exc:
            # Memory remains safe and a retry can complete cleanup idempotently.
            study.status = StudyStatus.failed
            await self._repository.save(study)
            raise StudyCompletionError("Study source cleanup is temporarily unavailable") from exc
        study.memory_object_key = memory_key
        study.memory_size_bytes = len(body)
        study.status = StudyStatus.completed
        await self._repository.save(study)
        return study

    async def delete(self, study: Study) -> None:
        study.status = StudyStatus.deleting
        await self._repository.save(study)
        try:
            for source in study.sources:
                await self._fsm.delete(key=source.object_key)
                if source.derived_object_key:
                    await self._fsm.delete(key=source.derived_object_key)
            if study.memory_object_key:
                await self._fsm.delete(key=study.memory_object_key)
        except (FsmConfigurationError, FsmStorageError) as exc:
            study.status = StudyStatus.failed
            await self._repository.save(study)
            raise StudyCompletionError("Study deletion is temporarily unavailable") from exc
        await self._repository.delete(study=study)

    async def memory(self, study: Study) -> dict:
        if not study.memory_object_key:
            raise StudyCompletionError("Study memory is not available")
        try:
            return json.loads((await self._fsm.get(key=study.memory_object_key)).decode())
        except (ValueError, UnicodeDecodeError, FsmConfigurationError, FsmStorageError) as exc:
            raise StudyCompletionError("Study memory is temporarily unavailable") from exc
