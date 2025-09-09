from src.domain.services.quiz_manager import QuizManager
from src.core.logs import info, warning
import json

quiz_handler = QuizManager()

async def get_context_from_app(item_name: str, input_identification: str, force_new_generation: bool, amount_question: int) -> dict:
    info(f"Fetching context for item: {item_name}, identification: {input_identification}")
    raw_input = quiz_handler.get_input(item_name=item_name, input_identification=input_identification)

    context = await quiz_handler.generate_context(
        item_name=item_name, 
        input_data=raw_input.get("input_data", ""), 
        input_identification=input_identification,
        force_new_generation=force_new_generation,
        amount_question=amount_question
        )

    try:
        raw_json_str = context['candidates'][0]['content']['parts'][0]['text']
        parsed = json.loads(raw_json_str)
        quiz_handler.save_questions(item_name=item_name, input_identification=input_identification, response=parsed)
    except KeyError as e:
        warning(f"Error parsing JSON: {e}")
        parsed = context

    # check/limit the number of questions to amount_question
    if "questions" in parsed and isinstance(parsed["questions"], list):
        parsed["questions"] = parsed["questions"][:amount_question]
    



    return parsed
    

