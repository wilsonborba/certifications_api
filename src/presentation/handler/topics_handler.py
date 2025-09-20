
from src.domain.services.quiz_api_manager import QuizAPIManager
from src.core.logs import info, debug, error

preview_manager = QuizAPIManager()


def get_topics_from_app(item_name, *, page: int, per_page: int):


    
    api_data = preview_manager.get_topics(item_name=item_name,
        page=page,
        per_page=per_page
    )
    
    return api_data