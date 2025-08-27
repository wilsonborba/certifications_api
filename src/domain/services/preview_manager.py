


from src.dal.local.db_adapter import DBAdapter


class PreviewManager:
    def __init__(self):
        self.db_adapter = DBAdapter()


    def get_all_sources(self):
        
        db_sources = self.db_adapter.read_all("accredit_sourceitem")

        list_of_sources = []

        for s in db_sources:
            source_name = s.get("source_name", None)
            if source_name:
                list_of_sources.append(source_name)

        return list_of_sources