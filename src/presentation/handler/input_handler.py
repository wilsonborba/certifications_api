from src.domain.services.quiz_api_manager import QuizAPIManager
from src.core.logs import info, debug, error

preview_manager = QuizAPIManager()

def get_input_from_app(item_name: str, input_identification: str) -> dict:
    
    info(f"Fetching input for item: {item_name}, identification: {input_identification}")
    result = preview_manager.get_input(item_name=item_name, input_identification=input_identification)

    return result