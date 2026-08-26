from __future__ import annotations

from datetime import datetime
from enum import StrEnum

from pydantic import BaseModel, Field


class StudyStatus(StrEnum):
    draft = "draft"
    collecting = "collecting"
    processing = "processing"
    ready = "ready"
    generating = "generating"
    completed = "completed"
    failed = "failed"
    deleting = "deleting"
    deleted = "deleted"


class SourceKind(StrEnum):
    pdf = "pdf"
    audio = "audio"
    text = "text"


class SourceStatus(StrEnum):
    uploading = "uploading"
    uploaded = "uploaded"
    processing = "processing"
    ready = "ready"
    failed = "failed"
    deleted = "deleted"


class SourceSelection(BaseModel):
    page_start: int | None = Field(default=None, ge=1)
    page_end: int | None = Field(default=None, ge=1)
    audio_start_ms: int | None = Field(default=None, ge=0)
    audio_end_ms: int | None = Field(default=None, ge=0)
    line_start: int | None = Field(default=None, ge=1)
    line_end: int | None = Field(default=None, ge=1)

    def validate_for(self, kind: SourceKind) -> None:
        pairs = ((self.page_start, self.page_end), (self.audio_start_ms, self.audio_end_ms), (self.line_start, self.line_end))
        if any(start is not None and end is not None and start > end for start, end in pairs):
            raise ValueError("Selection end must not precede its start")
        valid = {
            SourceKind.pdf: (self.page_start, self.page_end),
            SourceKind.audio: (self.audio_start_ms, self.audio_end_ms),
            SourceKind.text: (self.line_start, self.line_end),
        }[kind]
        if valid[0] is None or valid[1] is None:
            raise ValueError(f"A complete selection is required for {kind.value}")


class StudySource(BaseModel):
    id: str
    study_id: str
    owner_id: str
    kind: SourceKind
    filename: str
    content_type: str
    size_bytes: int = Field(ge=0)
    sha256: str = Field(min_length=64, max_length=64)
    object_key: str
    derived_object_key: str | None = None
    derived_size_bytes: int = Field(default=0, ge=0)
    status: SourceStatus = SourceStatus.uploaded
    selection: SourceSelection | None = None
    created_at: datetime
    updated_at: datetime


class Study(BaseModel):
    id: str
    owner_id: str
    name: str = Field(min_length=1, max_length=120)
    status: StudyStatus = StudyStatus.draft
    sources: list[StudySource] = Field(default_factory=list)
    created_at: datetime
    updated_at: datetime

    @property
    def active_size_bytes(self) -> int:
        return sum(
            source.size_bytes + source.derived_size_bytes
            for source in self.sources
            if source.status is not SourceStatus.deleted
        )
