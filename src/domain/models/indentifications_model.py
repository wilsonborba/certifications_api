from dataclasses import asdict, dataclass, field

@dataclass
class IdentificationsModel:
    input_identification: str
    title_identification: str | None = field(default=None)
    # description_identification: str | None = field(default=None)
    link_identification: str | None = field(default=None)
    img_link_identification: str | None = field(default=None)