

from src.domain.services.preview_manager import PreviewManager


preview_manager = PreviewManager()


def get_all_sources():
    return preview_manager.get_all_sources()