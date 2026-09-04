from __future__ import annotations

import json
import re
import subprocess
import tempfile
from uuid import uuid4

from lxml import etree
from pydantic import TypeAdapter, ValidationError

from src.dal.local.study_repository import StudyRepository
from src.dal.remote.fsm_media_adapter import FsmMediaAdapter, FsmStorageError
from src.domain.models.generation_policy import GenerationRequest
from src.domain.models.study import SourceStatus, Study, StudyStatus
from src.domain.models.study_question import D2Visual, GeneratedQuestionDocument, StudyQuestion, Visual
from src.domain.services.generation_policy_service import GenerationPolicyService


class QuestionContractError(RuntimeError):
    pass


_SVG_NS = "http://www.w3.org/2000/svg"


class QuestionGenerationService:
    def __init__(self, *, repository: StudyRepository, fsm: FsmMediaAdapter, policy: GenerationPolicyService) -> None:
        self._repository = repository
        self._fsm = fsm
        self._policy = policy

    def _chunk_text(self, text: str, max_chunk_chars: int = 3500) -> list[str]:
        paragraphs = text.split("\n\n")
        chunks: list[str] = []
        current: list[str] = []
        current_len = 0

        for p in paragraphs:
            p_str = p.strip()
            if not p_str:
                continue
            if current_len + len(p_str) > max_chunk_chars and current:
                chunks.append("\n\n".join(current))
                current = [p_str]
                current_len = len(p_str)
            else:
                current.append(p_str)
                current_len += len(p_str)

        if current:
            chunks.append("\n\n".join(current))
        return chunks or [text[:max_chunk_chars]]

    async def generate(
        self,
        *,
        user_id: str,
        study: Study,
        difficulty: str,
        idempotency_key: str,
        use_web: bool = False,
        question_count: int | None = None,
    ) -> list[StudyQuestion]:
        # A UI-picked cap, or None for "as many as the material supports"
        # (still bounded by the hard 20-question ceiling below).
        target_count = question_count if question_count and question_count > 0 else None
        max_questions = min(target_count, 20) if target_count else 20

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

        full_text = "\n\n".join(context_parts)
        chunks = self._chunk_text(full_text, max_chunk_chars=3500)
        # Target up to 4 chunks to keep total generation timely
        selected_chunks = chunks[:4]

        study.status = StudyStatus.generating
        await self._repository.save(study)

        collected_questions: list[StudyQuestion] = []
        last_error: str | None = None

        async def _save_progress(chunks_done: int, *, status: str = "generating") -> None:
            await self._repository.save_generation_progress(
                study_id=study.id,
                progress={
                    "status": status,
                    "chunks_done": chunks_done,
                    "chunks_total": len(selected_chunks),
                    "questions_generated": len(collected_questions),
                    "questions_target": target_count,
                },
            )

        await _save_progress(0)

        for idx, chunk in enumerate(selected_chunks):
            if target_count and len(collected_questions) >= target_count:
                break
            chunk_prompt = self._prompt(chunk, use_web=use_web)
            chunk_idempotency_key = f"{idempotency_key}-c{idx}"
            request = GenerationRequest(
                study_id=study.id,
                difficulty=difficulty,
                idempotency_key=chunk_idempotency_key,
                prompt=chunk_prompt,
                use_web=use_web,
                consume_credit=idx == 0,
            )
            outcome = await self._policy.generate(user_id=user_id, request=request)
            if outcome.status.value != "ready" or not outcome.response:
                last_error = outcome.error_code
                await _save_progress(idx + 1)
                continue
            try:
                web_references = self._extract_web_references(outcome.response)
                document = self._parse(outcome.response, tier=outcome.tier_requested or 0, web_references=web_references)
                collected_questions.extend(document.questions)
            except QuestionContractError as exc:
                last_error = str(exc)
            await _save_progress(idx + 1)

        study.status = StudyStatus.ready
        await self._repository.save(study)

        if not collected_questions:
            await _save_progress(len(selected_chunks), status="error")
            raise QuestionContractError(last_error or "Question generation is unavailable")

        # Deduplicate and cap at the requested count (or 20 when unbounded)
        seen_prompts: set[str] = set()
        final_questions: list[StudyQuestion] = []
        for q in collected_questions:
            normalized = q.prompt.strip().lower()
            if normalized not in seen_prompts:
                seen_prompts.add(normalized)
                final_questions.append(q)
                await self._repository.save_question(study_id=study.id, question=q)
                if len(final_questions) >= max_questions:
                    break

        await _save_progress(len(selected_chunks), status="ready")
        return final_questions

    @staticmethod
    def _prompt(context: str, *, use_web: bool = False) -> str:
        # Deliberately does NOT ask the model to self-report which URL it
        # used: an LLM asked to name its own source is prone to inventing or
        # misattributing one. Cortex already appends a deterministic
        # "References:" block of the exact URLs it actually searched (see
        # `_extract_web_references`/`format_web_references` in cortex), so
        # real citations are collected from that, not from the model.
        web_instructions = (
            """
A "## Web context" section may appear above with real web search results
gathered to help you answer accurately. Use it as background knowledge for
the questions below, same as the study material - you do not need to name
or cite it yourself, that is handled separately.
"""
            if use_web
            else ""
        )
        return f"""Study material:
{context}

Generate 3 to 10 educational multiple-choice questions based on the study material above.
Use a diagram (visual.kind: "d2") for EVERY question involving any of the following, even loosely:
a process, sequence, or cause-and-effect chain; a classification/categorization scheme (e.g. named
codes, levels, or categories and what distinguishes them); or the architecture/components of a
system, framework, or method (e.g. what parts it's made of, or how they connect). Most study
material has many such opportunities, so do not be shy about including one whenever it genuinely
applies - academic/technical material in particular is rarely "just a fact" once you look at how its
pieces relate. Set "edges" to at least 2 real arrows: each entry has "from_node" and "to_node" as
short node labels and "label" describing that connection (or "" if it doesn't need one) - never
output "kind": "d2" with an empty or missing "edges" list. Do not write diagram syntax yourself,
just the nodes and relationships. Only use "visual": {{"kind": "none"}} when the question is a
single isolated fact with no structure, category, or relationship of any kind to depict.
{web_instructions}You MUST output ONLY a valid JSON object matching this exact schema:

{{
  "questions": [
    {{
      "prompt": "What is the key concept or architectural relationship discussed?",
      "choices": [
        "Option A description",
        "Option B description",
        "Option C description",
        "Option D description"
      ],
      "correct_index": 0,
      "explanation": "Detailed explanation of why Option A is correct based on the text.",
      "citations": [
        {{
          "source": "Study material",
          "selection": "Quote or reference from text"
        }}
      ],
      "visual": {{
        "kind": "d2",
        "edges": [
          {{"from_node": "A", "to_node": "B", "label": "Data Flow"}},
          {{"from_node": "B", "to_node": "C", "label": "Processed"}}
        ],
        "description": "Concept diagram"
      }}
    }}
  ]
}}"""

    _WEB_REFERENCES_RE = re.compile(r"References:\s*\n((?:\s*\d+\.\s+\S+\s*\n?)+)", re.MULTILINE)

    @staticmethod
    def _extract_web_references(raw: str) -> list[str]:
        """Real, deterministic source URLs cortex actually searched, pulled
        from the "References:" block it appends to the response text (see
        `format_web_references` in cortex's executor/execution_service).
        This is independent of anything the model itself wrote."""
        match = QuestionGenerationService._WEB_REFERENCES_RE.search(raw)
        if not match:
            return []
        return [line.split(". ", 1)[1].strip() for line in match.group(1).splitlines() if ". " in line and line.strip()]

    @staticmethod
    def _parse(raw: str, *, tier: int, web_references: list[str] | None = None) -> GeneratedQuestionDocument:
        cleaned = raw.strip()
        # Strip markdown fences if present
        cleaned = re.sub(r"^```(?:json)?\s*|\s*```$", "", cleaned, flags=re.IGNORECASE).strip()
        # If the LLM returned leading/trailing conversational text around the JSON, extract the JSON object block
        match = re.search(r"\{[\s\S]*\}", cleaned)
        if match:
            cleaned = match.group(0)

        data = None
        try:
            data = json.loads(cleaned)
        except Exception:
            # Fallback: attempt to find individual question objects with regex if outer JSON is malformed
            q_matches = re.findall(r"\{\s*\"prompt\"[\s\S]*?\"visual\"[\s\S]*?\}", cleaned)
            salvaged: list[dict] = []
            for q_str in q_matches:
                try:
                    # Clean trailing commas if any
                    fixed_str = re.sub(r",\s*([\]}])", r"\1", q_str)
                    salvaged.append(json.loads(fixed_str))
                except Exception:
                    continue
            if salvaged:
                data = {"questions": salvaged}

        if not data or not isinstance(data, dict):
            from src.core.logs import error
            error(f"Failed to parse question JSON from LLM response. Raw response: {raw[:2000]!r}")
            raise QuestionContractError("The generated question contract was invalid")

        try:
            questions = data.get("questions", [])
            for item in questions:
                item["id"] = item.get("id") or str(uuid4())
                item["tier_requested"] = tier
                if not item.get("citations"):
                    item["citations"] = [{"source": "Study context", "selection": item.get("prompt", "")[:100]}]
                if web_references:
                    # Real URLs cortex actually searched while generating this
                    # chunk's questions - attached deterministically, not
                    # claimed by the model, which is why every question from
                    # this chunk gets the same (real) list rather than a
                    # per-question guess.
                    item["citations"] = item["citations"] + [
                        {"source": url, "selection": "Consulted via web search for this study material"}
                        for url in web_references
                    ]
                item["visual"] = QuestionGenerationService._sanitize_visual(item.get("visual"))
            return GeneratedQuestionDocument.model_validate({"questions": questions})
        except (ValueError, TypeError, ValidationError) as exc:
            from src.core.logs import error
            error(f"Question contract validation failed: {exc}. Raw model response: {raw[:2000]!r}")
            raise QuestionContractError("The generated question contract was invalid") from exc

    @staticmethod
    def _sanitize_visual(visual: dict | None) -> dict:
        """A malformed visual (e.g. the model still writing the old raw D2
        `source` string instead of the structured `edges` it was asked for,
        or any other schema slip) must not sink the 3-10 otherwise-valid
        questions in this chunk. Downgrade just the visual to "none" instead
        of letting the whole batch fail contract validation."""
        if not visual or visual.get("kind") not in ("latex", "d2"):
            return {"kind": "none"}
        try:
            TypeAdapter(Visual).validate_python(visual)
            return visual
        except ValidationError:
            return {"kind": "none"}

    @staticmethod
    def _escape_d2_text(text: str) -> str:
        """Makes arbitrary model-supplied text safe inside a double-quoted
        D2 identifier/label: escape backslashes and quotes, and collapse
        whitespace so one edge always stays on one D2 line."""
        text = text.replace("\\", "\\\\").replace('"', '\\"')
        return " ".join(text.split())

    @staticmethod
    def _build_d2_source(edges: list) -> str:
        """Builds D2 diagram syntax deterministically from the model's plain
        node/edge data (see `DiagramEdge`), instead of asking the model to
        write D2 DSL itself. The model only ever supplies short text
        fields - there is no diagram syntax for it to get wrong, and nothing
        resembling a D2 directive (import/exec/shell/URLs) can reach the
        rendered source, since every value is escaped into a quoted string.
        """
        lines = []
        for edge in edges:
            from_node = QuestionGenerationService._escape_d2_text(edge.from_node)
            to_node = QuestionGenerationService._escape_d2_text(edge.to_node)
            label = QuestionGenerationService._escape_d2_text(edge.label)
            if label:
                lines.append(f'"{from_node}" -> "{to_node}": "{label}"')
            else:
                lines.append(f'"{from_node}" -> "{to_node}"')
        return "\n".join(lines)

    @staticmethod
    def _flatten_nested_svg(svg_bytes: bytes) -> bytes:
        """d2 always wraps its diagram in an outer <svg> plus one inner
        <svg> carrying its own viewBox (used for the diagram's internal
        padding). Flutter's vector_graphics_compiler (what flutter_svg
        renders through) rejects that shape outright: "Unsupported nested
        <svg> element" - every single diagram fails to render as a result.
        Rewrite every non-root <svg> into an equivalent <g transform=
        "translate(...)"> so the exact same layout renders on Flutter.
        """
        root = etree.fromstring(svg_bytes)
        svg_tag = f"{{{_SVG_NS}}}svg"
        g_tag = f"{{{_SVG_NS}}}g"
        for elem in list(root.iter(svg_tag)):
            if elem is root:
                continue
            dx = dy = 0.0
            view_box = elem.get("viewBox")
            if view_box:
                parts = view_box.split()
                if len(parts) == 4:
                    dx, dy = -float(parts[0]), -float(parts[1])
            elem.tag = g_tag
            for attr in ("viewBox", "width", "height", "x", "y", "preserveAspectRatio"):
                elem.attrib.pop(attr, None)
            if dx or dy:
                existing = elem.get("transform", "")
                elem.set("transform", f"translate({dx} {dy}) {existing}".strip())
        return etree.tostring(root, xml_declaration=True, encoding="utf-8")

    @staticmethod
    def render_d2_svg(visual: D2Visual) -> bytes:
        source = QuestionGenerationService._build_d2_source(visual.edges)
        with tempfile.TemporaryDirectory(prefix="certifications-d2-") as directory:
            source_path = f"{directory}/diagram.d2"
            output_path = f"{directory}/diagram.svg"
            with open(source_path, "w", encoding="utf-8") as handle:
                handle.write(source)
            try:
                result = subprocess.run(["d2", "--layout=dagre", source_path, output_path], capture_output=True, timeout=15, check=False)
            except (FileNotFoundError, subprocess.TimeoutExpired) as exc:
                raise QuestionContractError("Diagram rendering is unavailable") from exc
            if result.returncode != 0:
                raise QuestionContractError("The generated diagram could not be rendered")
            with open(output_path, "rb") as handle:
                raw_svg = handle.read()
            try:
                return QuestionGenerationService._flatten_nested_svg(raw_svg)
            except etree.XMLSyntaxError:
                # Fall back to the raw d2 output rather than 503ing a
                # diagram that a browser (the shared-quiz view) could still
                # render even though Flutter can't.
                return raw_svg
