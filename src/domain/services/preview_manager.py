


from datetime import datetime, timezone
from src.domain.models.trends_model import TrendsModel
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
    
    def get_trends(
        self,
        item_name: str,
        *,
        page: int = 1,
        per_page: int = 45,
        kinds: list[str] | None = None,
        time_window: str | None = None,  # used for kind="top"
        ) -> dict[str, any]:
        """
        Build a numeric page by walking Reddit's cursor pagination per kind.
        Stateless: walks (page-1) times to reach the page for each kind.
        """
        assert page >= 1, "page must be >= 1"
        assert per_page >= 1, "per_page must be >= 1"

        adapter = self.adapters_factory.get_adapter(item_name)
        kinds = kinds or ["top", "hot", "communities"]

        # Even split; last bucket gets the remainder
        base = per_page // len(kinds)
        remainder = per_page % len(kinds)
        limits = [base + (1 if i < remainder else 0) for i in range(len(kinds))]

        results = []
        has_more_map: dict[str, bool] = {}

        for idx, kind in enumerate(kinds):
            limit = limits[idx]
            after = None

            # Walk to the requested numeric page for this kind
            for _ in range(page - 1):
                res = adapter.get_trends(kind=kind, limit=limit, after=after, time_window=time_window)
                after = res.get("after")
                if not after:
                    break  # no more pages for this kind

            # Now fetch the page we actually want
            res = adapter.get_trends(kind=kind, limit=limit, after=after, time_window=time_window)
            items = res.get("items", [])
            results.extend(items)
            has_more_map[kind] = bool(res.get("after"))

        # return {
        #     "page": page,
        #     "per_page": per_page,
        #     "kinds": kinds,
        #     "items": results,        # flat list (already mixed in order: top, hot, communities)
        #     "has_more": has_more_map # tells the client whether “next page” likely exists per kind
        # }

        return TrendsModel(
            item_name=item_name,
            page=page,
            per_page=per_page,
            kinds=kinds,
            trends=results,
            has_more=has_more_map,
            updated_at=datetime.now(timezone.utc).isoformat(),
        ).to_dict()
        
