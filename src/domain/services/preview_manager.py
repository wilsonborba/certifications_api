


from src.dal.remote.factory import AdapterFactory
from src.dal.local.db_adapter import DBAdapter


class PreviewManager:
    def __init__(self):
        self.db_adapter = DBAdapter()
        self.adapters_factory = AdapterFactory()


    def get_all_sources(self):
        
        db_sources = self.db_adapter.read_all("accredit_sourceitem")

        list_of_sources = []

        for s in db_sources:
            source_name = s.get("source_name", None)
            if source_name:
                list_of_sources.append(source_name)

        return list_of_sources
    
    def get_source(self, source_name):

        db_source = self.db_adapter.read_by_id(
            "accredit_sourceitem", 
            source_name, 
            id_column="source_name"
        )

        return db_source
    
    def get_item_preview(self, source_name):
        adapter = self.adapters_factory.get_adapter(source_name)
        preview = adapter.get_preview()
        return preview
    

    def get_all_items(self):
        db_items = self.db_adapter.read_all("accredit_sourceitem")

        list_of_items = []

        for i in db_items:
            item_name = i.get("item_name", None)
            if item_name:
                list_of_items.append(item_name)

        return list_of_items
    
    def get_item(self, item_name):
        
        db_item = self.db_adapter.read_by_id(
            "accredit_sourceitem", 
            item_name, 
            id_column="item_name"
        )

        return db_item
    
