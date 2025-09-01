


from datetime import datetime, timezone
from src.dal.remote.gemini import GeminiClient
from src.domain.models.input_model import InputModel
from src.domain.models.topics_model import TopicModel
from src.dal.remote.factory import AdapterFactory
from src.dal.local.db_adapter import DBAdapter
from src.core.logs import error, debug

class QuizManager:
    def __init__(self):
        self.db_adapter = DBAdapter()
        self.adapters_factory = AdapterFactory()
        self.gemini_client = GeminiClient()


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
    
    def get_item_preview(self, item_name):
        adapter = self.adapters_factory.get_adapter(item_name)
        if not adapter:
            return None
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
    
    def get_topics(
        self,
        item_name: str,
        *,
        page: int = 1,
        per_page: int = 45,
        **adapter_kwargs,   # adapter-specific knobs if needed (e.g., time_window, tagged)
    ) -> dict[str, any]:
        adapter = self.adapters_factory.get_adapter(item_name)
        
        if not adapter:
            error(f"No adapter found for source: {item_name}")
            return TopicModel(
                item_name=item_name,
                page=page,
                per_page=per_page,
                topics=[],
                has_more=False,
                updated_at=datetime.now(timezone.utc).isoformat(),
                source_name=None,
            ).to_dict()

        res = adapter.get_topics(page=page, per_page=per_page, **adapter_kwargs)

        return TopicModel(
            item_name=res.get("item_name", item_name),
            page=res.get("page", page),
            per_page=res.get("per_page", per_page),
            topics=res.get("topics", []),
            has_more=bool(res.get("has_more")),
            updated_at=res.get("updated_at", datetime.now(timezone.utc).isoformat()),
            source_name=res.get("source_name"),
        ).to_dict()
    
    def get_input(
        self,
        item_name: str,
        input_identification: str,
        **adapter_kwargs,   # adapter-specific knobs if needed (e.g., time_window, tagged)
    ) -> dict[str, any]:
        adapter = self.adapters_factory.get_adapter(item_name)
        
        if not adapter:
            error(f"No adapter found for source: {item_name}")
            return InputModel(
                source_name=item_name,
                item_name=item_name,
                input_identification=input_identification,
                input_data=None,
                updated_at=datetime.now(timezone.utc).isoformat(),
            ).to_dict()

        res = adapter.get_input(input_identification=input_identification, **adapter_kwargs)

        return InputModel(
            source_name=item_name,
            item_name=item_name,
            input_identification=input_identification,
            input_data=res.get("input_data", None),
            updated_at=res.get("updated_at", datetime.now(timezone.utc).isoformat()),
        ).to_dict()
    
    async def generate_context(
            self,
            
            item_name: str,
            input_data: dict[str, any],
            *args,
            **kwargs
    ) -> dict[str, any]:
        adapter = self.adapters_factory.get_adapter(item_name)

        if not adapter:
            error(f"No adapter found for source: {item_name}")
            return {"error": "No adapter found"}
        
        
        
        prompt = adapter.generate_context(input_data=input_data, *args, **kwargs)

        
        response = await self.gemini_client.generate_text(
            prompt=prompt,
            system_instruction=adapter.instructions(),
            response_mime_type="application/json",
            temperature=0.7,
        )

        return response