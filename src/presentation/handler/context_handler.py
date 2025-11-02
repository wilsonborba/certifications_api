import time
from src.presentation.handler.responses import NotEnoughQuestionsGeneratedError
from src.domain.services.quiz_api_manager import QuizAPIManager
from src.core.logs import info, warning, error, debug
import json
import random

quiz_handler = QuizAPIManager()


async def generate_and_save_questions(
        user_uuid_id: str,
        item_name: str,  
        input_identification: str, 
        force_new_generation: bool, 
        amount_question: int,
        selected_language: str
    ) -> list[dict]:

    raw_input = quiz_handler.get_input(item_name=item_name, input_identification=input_identification)


    context = await quiz_handler.generate_context(
        item_name=item_name,
        input_data=raw_input.get("input_data", ""),
        input_identification=input_identification,
        force_new_generation=force_new_generation,
        amount_question=amount_question,
        selected_language=selected_language
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


        
        if "questions" in context and "candidates" not in context:
            saved_questions = context["questions"]

        else:
            debug(f"{'questions' in context} not found, trying candidates...")
            raw_json_str = context['candidates'][0]['content']['parts'][0]['text']
            parsed = json.loads(raw_json_str)
            result = quiz_handler.save_questions(
                item_name=item_name, 
                input_identification=input_identification, 
                response=parsed, 
                selected_language=selected_language
                )
            saved_questions = [q for q in result.saved_questions]

        return saved_questions


        

    except KeyError as e:
        warning(f"Error parsing JSON: {e}")
        debug(f"Raw context: {context}")
        return None
    



async def get_context_from_app(
    item_name: str, 
    input_identification: str, 
    force_new_generation: bool, 
    amount_question: int,
    user_uuid_id: str,
    selected_language: str
) -> list[dict]:
    info(f"Fetching context for item: {item_name}, identification: {input_identification}")
    
    saved_questions = await generate_and_save_questions(
        user_uuid_id=user_uuid_id,
        item_name=item_name,
        input_identification=input_identification,
        force_new_generation=force_new_generation,
        amount_question=amount_question,
        selected_language=selected_language
    )

    if not saved_questions:
        error(f"No saved questions found for item: {item_name}, identification: {input_identification}")
        return None

    # Not enough? Try generating new ones
    if len(saved_questions) < amount_question:
        warning(f"Only {len(saved_questions)} questions were saved, less than requested {amount_question}")

        force_new_generation = True
        new_saved_questions = await generate_and_save_questions(
            user_uuid_id=user_uuid_id,
            item_name=item_name,
            input_identification=input_identification,
            force_new_generation=force_new_generation,
            amount_question=amount_question,
            selected_language=selected_language
        )

        if not new_saved_questions:
            error(f"Failed to generate enough questions for {item_name}/{input_identification}")
            raise NotEnoughQuestionsGeneratedError(
                f"Could not generate enough questions for item: {item_name}, identification: {input_identification}"
            )

        # Combine both
        saved_questions += new_saved_questions

    #  Shuffle and then limit to requested amount
    random.shuffle(saved_questions)
    saved_questions = saved_questions[:amount_question]

    return saved_questions


def save_complaint(
    complaint_text: str,
    question_id: str,
    user_uuid_id: str
    ) -> dict:
    
    quiz_pdf_manager = QuizAPIManager()
    complaint_result = quiz_pdf_manager.save_complaint(
        user_uuid_id=user_uuid_id,
        complaint_text=complaint_text,
        question_id=question_id
    )
    return complaint_result
    

