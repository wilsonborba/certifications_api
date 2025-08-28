from dataclasses import asdict, dataclass, field

@dataclass
class TopicModel:
    source_name: str
    item_name: str
    page: int 
    per_page: int 
    topics: list[dict] 
    has_more:  dict[str, bool]
    updated_at: str | None = None

    def to_dict(self):
        return asdict(self)