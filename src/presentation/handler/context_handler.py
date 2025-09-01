from src.domain.services.quiz_manager import QuizManager
from src.core.logs import info, debug, error
import json

quiz_handler = QuizManager()

async def get_context_from_app(item_name: str, input_identification: str) -> dict:
    info(f"Fetching context for item: {item_name}, identification: {input_identification}")
    raw_input = quiz_handler.get_input(item_name=item_name, input_identification=input_identification)

    context = await quiz_handler.generate_context(item_name=item_name, input_data=raw_input.get("input_data", ""))

    raw_json_str = context['candidates'][0]['content']['parts'][0]['text']
    parsed = json.loads(raw_json_str)

    return parsed