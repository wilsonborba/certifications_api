
from __future__ import annotations

from abc import ABC, abstractmethod
from typing import Any, Dict, List, Type

from src.domain.models.preview_model import PreviewModel


class BaseAdapter(ABC):


    
    def context_output_structure(self, amount_question: int) -> str:
        
        context = f"\nGenerate {amount_question} quiz questions in JSON format."
        context += "Output must be in this exact JSON structure:\n"
        context += """
            {
            "questions": [
                {
                "question": "...",
                "correct_answer": "...",
                "options": ["...", "...", "...", "..."],
                "justification": "..."
                }
            ]
            }
            """
        
        return context

    @abstractmethod
    def get_preview(self) -> PreviewModel:
        pass

    @abstractmethod
    def get_topics(
        self,
        *,
        page: int = 1,          # numeric, 1-based
        per_page: int = 30,     # unified page size
        **kwargs: Any           # adapter-specific (e.g., time_window, tagged, etc.)
    ) -> Dict[str, Any]:
        """
        MUST return the unified structure:

        {
          "trends": [ ... normalized objects ... ],
          "page": <int>,
          "per_page": <int>,
          "has_more": <bool>,              # whether page+1 likely exists
          "updated_at": <iso8601>,
          "item_name": <adapter item_name>,
          "source_name": <adapter source_name>
        }

        NO adapter-specific fields here. Keep those internal.
        """
        ...



