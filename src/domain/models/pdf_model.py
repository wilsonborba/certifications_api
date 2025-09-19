from __future__ import annotations
from dataclasses import dataclass
from typing import Optional, List, Dict, Any, Tuple


# ---------- Low-level metadata & scans ----------

@dataclass(slots=True, frozen=True)
class PdfMetadata:
    filename: str
    pages: int
    title: Optional[str]
    author: Optional[str]
    subject: Optional[str]
    creation_date: Optional[str]
    mod_date: Optional[str]
    producer: Optional[str]
    encrypted: bool

    def to_dict(self) -> Dict[str, Any]:
        return {
            "filename": self.filename,
            "pages": self.pages,
            "title": self.title,
            "author": self.author,
            "subject": self.subject,
            "creation_date": self.creation_date,
            "mod_date": self.mod_date,
            "producer": self.producer,
            "encrypted": self.encrypted,
        }


@dataclass(slots=True, frozen=True)
class AntivirusScan:
    engine: str
    status: str                 # CLEAN | FOUND | UNKNOWN | UNAVAILABLE
    signature: Optional[str]

    def to_dict(self) -> Dict[str, Any]:
        return {"engine": self.engine, "status": self.status, "signature": self.signature}


@dataclass(slots=True, frozen=True)
class YaraScan:
    engine: str = "yara"
    status: str = "SKIPPED"     # SKIPPED | MATCH | CLEAN | ERROR
    matches: List[str] = ()

    def to_dict(self) -> Dict[str, Any]:
        return {"engine": self.engine, "status": self.status, "matches": list(self.matches)}


# ---------- Parsed content ----------

@dataclass(slots=True, frozen=True)
class ElementAttrs:
    bbox: Optional[Tuple[float, float, float, float]] = None
    level: Optional[int] = None
    table_markdown: Optional[str] = None
    ocr: Optional[bool] = None
    ocr_confidence: Optional[float] = None

    def to_dict(self) -> Dict[str, Any]:
        d: Dict[str, Any] = {}
        if self.bbox is not None:
            d["bbox"] = list(self.bbox)
        if self.level is not None:
            d["level"] = self.level
        if self.table_markdown:
            d["table_markdown"] = self.table_markdown
        if self.ocr is not None:
            d["ocr"] = self.ocr
        if self.ocr_confidence is not None:
            d["ocr_confidence"] = self.ocr_confidence
        return d


@dataclass(slots=True, frozen=True)
class PdfElement:
    type: str
    text: str
    page: Optional[int]
    attrs: ElementAttrs = ElementAttrs()

    def to_dict(self) -> Dict[str, Any]:
        return {
            "type": self.type,
            "text": self.text,
            "page": self.page,
            "attrs": self.attrs.to_dict(),
        }


@dataclass(slots=True, frozen=True)
class PdfChunk:
    chunk_id: str
    text: str
    pages: List[int]
    section_path: List[str]
    tokens_est: int

    def to_dict(self) -> Dict[str, Any]:
        return {
            "chunk_id": self.chunk_id,
            "text": self.text,
            "pages": list(self.pages),
            "section_path": list(self.section_path),
            "tokens_est": self.tokens_est,
        }


@dataclass(slots=True, frozen=True)
class ParsedDocument:
    document_id: str
    source: str
    elements: List[PdfElement]
    chunks: List[PdfChunk]

    def to_dict(self) -> Dict[str, Any]:
        return {
            "document_id": self.document_id,
            "source": self.source,
            "elements": [e.to_dict() for e in self.elements],
            "chunks": [c.to_dict() for c in self.chunks],
        }
