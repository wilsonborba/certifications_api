
from src.domain.services.preview_manager import PreviewManager
from src.core.logs import info, debug, error

preview_manager = PreviewManager()


def get_trends_from_app(item_name, *, page: int, per_page: int, kinds: list[str] | None, time_window: str | None):


    
    api_data = preview_manager.get_trends(item_name=item_name,
        page=page,
        per_page=per_page,
        kinds=kinds,
        time_window=time_window,
    )
    
    return api_data