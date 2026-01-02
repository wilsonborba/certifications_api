from src.dal.remote.ai.gemini import GeminiClient
from src.dal.remote.ai.groq import GroqClient


class AiFactory:
    ai_adapters = {
        "gemini": GeminiClient,
        "groq": GroqClient,
        # "chatgpt": ChatGPTAdapter,
        # "claude": ClaudeAdapter,
    }

    @classmethod
    def get_adapter(cls, ai_name: str):
        adapter_class = cls.ai_adapters.get(ai_name.lower())
        if not adapter_class:
            raise ValueError(f"No AI adapter found for: {ai_name}")
        return adapter_class()
