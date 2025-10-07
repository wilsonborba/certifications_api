import time
from src.domain.services.quiz_api_manager import QuizAPIManager
from src.core.logs import info, warning
import json
from src.core.logs import debug

quiz_handler = QuizAPIManager()

async def get_context_from_app(
        item_name: str, 
        input_identification: str, 
        force_new_generation: bool, 
        amount_question: int,
        user_uuid_id: str
    ) -> dict:
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
        status_code = quiz_handler.gemini_client.last_status_code or 200
        attempts = quiz_handler.gemini_client.last_attempts or 1
        latency_ms = quiz_handler.gemini_client.last_latency_ms or 0.0

        user_usage_tracking =quiz_handler.save_ai_user_usage(
            user_uuid_id=user_uuid_id,
            raw_context=context,
            status_code=status_code,
            attempts=attempts,
            latency_ms=latency_ms,
            source_item_name=item_name,
            source_input_identification=input_identification,
            is_for_pdf=False
        )
        
        info(f"User usage tracking saved: {user_usage_tracking}")

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
    

