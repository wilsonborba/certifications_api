


from src.dal.remote.reddit_adapter import RedditAdapter


class AdapterFactory:

    adapters = {
        "reddit": RedditAdapter,
    }

    @classmethod
    def get_adapter(cls, source_name: str):
        adapter_class = cls.adapters.get(source_name.lower())
        if not adapter_class:
            raise ValueError(f"No adapter found for source: {source_name}")
        return adapter_class()