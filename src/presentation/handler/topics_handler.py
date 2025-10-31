
from src.domain.services.quiz_api_manager import QuizAPIManager
from src.core.logs import info, debug, error

preview_manager = QuizAPIManager()


def get_topics_from_app(item_name, *, page: int, per_page: int):


    
    api_data = preview_manager.get_topics(item_name=item_name,
        page=page,
        per_page=per_page
    )
    
    return api_data


async def add_new_topic_request_to_db(app_url: str, user_uuid_id: str) -> None:
    """
    Adds a new topic request to the database.
    """
    try:
        # Simulate database insertion logic
        info(f"Adding new topic request for URL: {app_url}")
        return preview_manager.save_solicitation_new_topic(app_url=app_url, user_uuid_id=user_uuid_id)

    except Exception as e:
        error(f"Failed to add new topic request for URL '{app_url}': {e}")
        raise