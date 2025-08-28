from dataclasses import asdict, dataclass, field

@dataclass
class TrendsModel:
    item_name: str
    page: int 
    per_page: int
    kinds: list[str] 
    trends: list[dict] 
    has_more:  dict[str, bool]
    updated_at: str | None = None

    def to_dict(self):
        return asdict(self)