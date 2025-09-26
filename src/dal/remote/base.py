
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
                "question": "string",
                "correct_answer": "string",
                "options": ["string", "string", "string", "string"],
                "justification": "string",
                "difficulty": integer (1 to 6, based on Bloom’s Taxonomy levels)
                }
            ]
            }
            """
        
        return context
    
    def instructions(self) -> str:
        pass

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

    @abstractmethod
    def get_input(self) -> Dict[str, Any]:
        """
        Fetch the canonical input for a specific topic.
        """
        ...

    @abstractmethod
    def search(self, *args, **kwargs) -> List[Dict[str, Any]]:
        """
        Optional: Search within the adapter's data.
        """
        return []
    
    @abstractmethod
    def generate_context(self, input_data: Dict[str, Any], amount_question) -> str:
        """
        Generate a context string from the input data.
        """
        ...

    def _simple_fuzzy_score(self, text: str, query: str) -> float:
        if not text or not query:
            return 0.0
        if query in text:
            return min(1.0, 0.6 + len(query) / max(len(text), len(query)))
        pref = 1.0 if text.startswith(query) else 0.0
        suff = 1.0 if text.endswith(query) else 0.0
        best = 0; qlen = len(query)
        for w in range(min(qlen, 8), 1, -1):
            if any(query[i:i+w] in text for i in range(0, qlen - w + 1)):
                best = w; break
        base = best / qlen
        return min(1.0, 0.15 + 0.35 * base + 0.25 * pref + 0.25 * suff)

    

        
