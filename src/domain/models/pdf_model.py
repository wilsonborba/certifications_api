from dataclasses import dataclass
from typing import Dict, List, Optional, Literal

Status = Literal["CLEAN", "SUSPECT", "MALICIOUS", "ERROR"]

@dataclass(slots=True, frozen=True)
class AiInjectionReport:
    engine: str                 # e.g., "gemini-2.5-flash"
    status: Status              # "CLEAN" | "SUSPECT" | "MALICIOUS" | "ERROR"
    confidence: float           # 0.0 - 1.0
    reasons: List[str]
    indicators: List[str]       # patterns / signals
    pages: List[int]            # best-effort page mapping
    quotes: List[str]           # short snippets
    latency_ms: int
    model: Optional[str] = None
    raw: Optional[Dict[str, object]] = None  # raw parsed JSON (optional)

    def to_dict(self) -> Dict[str, object]:
        return {
            "engine": self.engine,
            "status": self.status,
            "confidence": self.confidence,
            "reasons": list(self.reasons),
            "indicators": list(self.indicators),
            "pages": list(self.pages),
            "quotes": list(self.quotes),
            "latency_ms": self.latency_ms,
            "model": self.model,
            "raw": self.raw,
        }

    def is_blocking(self) -> bool:
        """Convenience for routes to decide whether to block."""
        return self.status in ("MALICIOUS", "SUSPECT")