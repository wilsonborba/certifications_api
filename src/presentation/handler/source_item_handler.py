
from src.dal.remote.factory import AdapterFactory
from src.domain.services.quiz_manager import QuizManager
from src.core.logs import info, debug

preview_manager = QuizManager()

factory = AdapterFactory()

def get_all_sources_data():
    return set(preview_manager.get_all_sources())


def get_all_item_data():
    # db_data = preview_manager.get_all_items()

    all_items = []

    for f in factory.adapters.keys():
        db_data = preview_manager.get_item(f)
        if not db_data:
            info(f"Source '{f}' not found in the database.")
            preview = preview_manager.get_item_preview(f)
            if preview:
                data_to_insert = preview.to_dict()
                
                result = preview_manager.db_adapter.insert_row(
                "accredit_sourceitem",
                data_to_insert
                )


                data_to_insert["id"] = result[0] 
                all_items.append(data_to_insert)

            else:
                continue
        else:
            all_items.append(db_data)
    
    return all_items
                

    


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
