from dataclasses import asdict, dataclass, field


class EnumMode:
    PLAYFUL = "playful"
    SERIOUS = "serious"
    BOTH = "both"


@dataclass
class PreviewModel:
    mode: str
    source_name: str
    has_topic: bool
    item_name: str
    item_img: str
    updated_at: str

    def to_dict(self):
        return asdict(self)