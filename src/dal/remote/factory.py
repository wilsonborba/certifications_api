


from src.dal.remote.reddit_adapter import RedditAdapter
from src.core.logs import error

class AdapterFactory:

    adapters = {
        "reddit": RedditAdapter,
    }

    @classmethod
    def get_adapter(cls, item_name: str):
        adapter_class = cls.adapters.get(item_name.lower())
        if not adapter_class:
            error(f"No adapter found for source: {item_name}")
            return None
        return adapter_class()