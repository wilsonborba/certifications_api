

import datetime
from src.domain.services.preview_manager import PreviewManager
from src.core.logs import info

preview_manager = PreviewManager()



def get_all_sources_data():
    return preview_manager.get_all_sources()


def get_all_item_data():
    return preview_manager.get_all_items()


def get_specific_source_data(source_name):
    db_data = preview_manager.get_source(source_name)
    
    return db_data

def get_specific_item_data(item_name):

    db_data = preview_manager.get_item(item_name)

    if not db_data:
        info(f"Source '{item_name}' not found in the database.")
        preview = preview_manager.get_item_preview(item_name)
        if preview:
            preview_manager.db_adapter.insert_row(
            "accredit_sourceitem",
            preview.to_dict()
            )
            return preview.to_dict()
        else:
            return None
    
    return db_data
