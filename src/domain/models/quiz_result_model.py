from dataclasses import asdict, dataclass, field
from typing import List

@dataclass
class QuizResultModel:
    saved_questions: List[dict]
    identification: str | None = None
    created_at: str | None = None

    def to_dict(self):
        return asdict(self)