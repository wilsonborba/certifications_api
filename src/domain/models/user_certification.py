from dataclasses import asdict, dataclass


@dataclass
class UserCertificationModel:
    certification_title: str | None = None
    uuid_certification: str | None = None
    user_uuid_id: str | None = None
    full_name: str | None = None
    language: str | None = None
    time_spent: str | None = None
    created_at: str | None = None
    is_pdf: bool | None = None
    score: float | None = None
    total_questions: int | None = None
    correct_questions: int | None = None
    wrong_questions: int | None = None

    def to_dict(self):
        return asdict(self)
