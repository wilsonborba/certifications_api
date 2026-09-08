from __future__ import annotations

from typing import Literal

from pydantic import BaseModel, Field, model_validator


class NoVisual(BaseModel):
    kind: Literal["none"] = "none"


class LatexVisual(BaseModel):
    kind: Literal["latex"] = "latex"
    source: str = Field(min_length=1, max_length=10_000)
    description: str = Field(min_length=1, max_length=500)


class DiagramEdge(BaseModel):
    """One arrow in a concept/workflow diagram. The model only ever supplies
    this plain data - the D2 source text itself is built deterministically
    from it server-side (see `render_d2_svg`), so there is no free-form DSL
    for the model to get wrong or for us to have to sanitize against
    injection."""

    from_node: str = Field(min_length=1, max_length=80)
    to_node: str = Field(min_length=1, max_length=80)
    label: str = Field(default="", max_length=120)


class D2Visual(BaseModel):
    kind: Literal["d2"] = "d2"
    edges: list[DiagramEdge] = Field(min_length=1, max_length=12)
    description: str = Field(min_length=1, max_length=500)


Visual = NoVisual | LatexVisual | D2Visual


class StudyQuestion(BaseModel):
    id: str
    prompt: str = Field(min_length=1, max_length=20_000)
    choices: list[str] = Field(min_length=2, max_length=6)
    correct_index: int = Field(ge=0)
    explanation: str = Field(min_length=1, max_length=10_000)
    citations: list[dict[str, object]] = Field(min_length=1, max_length=50)
    visual: Visual = Field(default_factory=NoVisual)
    tier_requested: int = Field(ge=0, le=5)
    schema_version: Literal[1] = 1

    @model_validator(mode="after")
    def correct_answer_must_exist(self) -> "StudyQuestion":
        if self.correct_index >= len(self.choices):
            raise ValueError("correct_index must identify one choice")
        return self

    def for_answering(self) -> dict[str, object]:
        payload = self.model_dump(mode="json")
        payload.pop("correct_index")
        payload.pop("explanation")
        return payload


class GeneratedQuestionDocument(BaseModel):
    questions: list[StudyQuestion] = Field(min_length=1, max_length=50)
