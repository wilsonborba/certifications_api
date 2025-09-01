from dataclasses import asdict, dataclass, field

@dataclass
class InputModel:
    source_name: str
    item_name: str
    input_identification: str 
    input_data: dict 
    updated_at: str | None = None

    def to_dict(self):
        return asdict(self)