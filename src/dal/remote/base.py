
from __future__ import annotations

from abc import ABC, abstractmethod
from typing import Any, Dict, List, Type

from src.domain.models.preview_model import PreviewModel


class BaseAdapter(ABC):

    @abstractmethod
    def get_preview(self) -> PreviewModel:
        pass



