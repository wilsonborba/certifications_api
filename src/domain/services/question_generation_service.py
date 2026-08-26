from __future__ import annotations

import json
import re
import subprocess
import tempfile
from uuid import uuid4

from src.dal.local.study_repository import StudyRepository
from src.dal.remote.fsm_s3_adapter import FsmS3Adapter, FsmStorageError
from src.domain.models.generation_policy import GenerationRequest
from src.domain.models.study import SourceStatus, Study, StudyStatus
from src.domain.models.study_question import D2Visual, GeneratedQuestionDocument, StudyQuestion
from src.domain.services.generation_policy_service import GenerationPolicyService


class QuestionContractError(RuntimeError):
    pass


class QuestionGenerationService:
    def __init__(self, *, repository: StudyRepository, fsm: FsmS3Adapter, policy: GenerationPolicyService) -> None:
        self._repository = repository
        self._fsm = fsm
        self._policy = policy

    async def generate(self, *, user_id: str, study: Study, difficulty: str, idempotency_key: str) -> list[StudyQuestion]:
        ready_sources = [source for source in study.sources if source.status is SourceStatus.ready and source.derived_object_key]
        if not ready_sources:
            raise QuestionContractError("Selected sources must finish processing before questions can be generated")
        context_parts: list[str] = []
        for source in ready_sources:
            try:
                artifact = json.loads((await self._fsm.get(key=source.derived_object_key or "")).decode())
            except (ValueError, UnicodeDecodeError, FsmStorageError) as exc:
                raise QuestionContractError("Selected study context is unavailable") from exc
            context_parts.append(artifact.get("text", ""))
        prompt = self._prompt("\n\n".join(context_parts)[:100_000])
        request = GenerationRequest(study_id=study.id, difficulty=difficulty, idempotency_key=idempotency_key, prompt=prompt)
        study.status = StudyStatus.generating
        await self._repository.save(study)
        outcome = await self._policy.generate(user_id=user_id, request=request)
        if outcome.status.value != "ready" or not outcome.response:
            study.status = StudyStatus.ready
            await self._repository.save(study)
            raise QuestionContractError(outcome.error_code or "Question generation is unavailable")
        document = self._parse(outcome.response, tier=outcome.tier_requested or 0)
        study.status = StudyStatus.ready
        await self._repository.save(study)
        for question in document.questions:
            await self._repository.save_question(study_id=study.id, question=question)
        return document.questions

    @staticmethod
    def _prompt(context: str) -> str:
        return """Create multiple-choice educational questions only from the supplied study context.
Return valid JSON with exactly this top-level shape: {\"questions\":[...]}. Each question needs prompt, choices (2-6), correct_index, explanation, citations, and visual. visual is either {\"kind\":\"none\"}, {\"kind\":\"latex\",\"source\":\"...\",\"description\":\"...\"}, or {\"kind\":\"d2\",\"source\":\"...\",\"description\":\"...\"}. Cite source selections. Do not use Markdown fences or prose outside JSON.

Study context:
""" + context

    @staticmethod
    def _parse(raw: str, *, tier: int) -> GeneratedQuestionDocument:
        candidate = re.sub(r"^```(?:json)?\s*|\s*```$", "", raw.strip(), flags=re.IGNORECASE)
        try:
            data = json.loads(candidate)
            for item in data.get("questions", []):
                item["id"] = item.get("id") or str(uuid4())
                item["tier_requested"] = tier
                visual = item.get("visual") or {"kind": "none"}
                if visual.get("kind") == "d2":
                    QuestionGenerationService._validate_d2(str(visual.get("source", "")))
            return GeneratedQuestionDocument.model_validate(data)
        except (ValueError, TypeError) as exc:
            raise QuestionContractError("The generated question contract was invalid") from exc

    @staticmethod
    def _validate_d2(source: str) -> None:
        if not source.strip() or re.search(r"(?i)(\bimport\b|https?://|file://|\bexec\b|\bshell\b)", source):
            raise QuestionContractError("The generated diagram was invalid")

    @staticmethod
    def render_d2_svg(visual: D2Visual) -> bytes:
        QuestionGenerationService._validate_d2(visual.source)
        with tempfile.TemporaryDirectory(prefix="certifications-d2-") as directory:
            source_path = f"{directory}/diagram.d2"
            output_path = f"{directory}/diagram.svg"
            with open(source_path, "w", encoding="utf-8") as handle:
                handle.write(visual.source)
            try:
                result = subprocess.run(["d2", "--layout=dagre", source_path, output_path], capture_output=True, timeout=15, check=False)
            except (FileNotFoundError, subprocess.TimeoutExpired) as exc:
                raise QuestionContractError("Diagram rendering is unavailable") from exc
            if result.returncode != 0:
                raise QuestionContractError("The generated diagram could not be rendered")
            with open(output_path, "rb") as handle:
                return handle.read()
