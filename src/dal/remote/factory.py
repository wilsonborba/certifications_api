


from src.dal.remote.aws.aws_infra_catalog_adapter import  AwsInfraCatalogAdapter # AwsWhitepaperServicesAdapter
from src.dal.remote.killedbygoogle_adapter import KilledByGoogleAdapter
from src.dal.remote.companies_marketcap_adapter import CompaniesMarketCapAdapter
from src.dal.remote.hackernews_adapter import HackerNewsAdapter
from src.dal.remote.chesscom_adapter import ChessComAdapter
from src.dal.remote.stackexchange_adapter import StackExchangeOverflowAdapter
from src.dal.remote.reddit_adapter import RedditAdapter
from src.core.logs import error

class AdapterFactory:

    adapters = {
        "reddit": RedditAdapter,
        "stack_exchange_overflow": StackExchangeOverflowAdapter,
        "chess_com": ChessComAdapter,
        "hacker_news": HackerNewsAdapter,
        # "meetup": MeetupAdapter,  # Placeholder for future implementation (waiting for API access)
        "companies_marketcap": CompaniesMarketCapAdapter,
        "killed_by_google": KilledByGoogleAdapter,
        # "aws_whitepaper_services": AwsWhitepaperServicesAdapter # have a api that is better than scraping
        "aws_infra_catalog": AwsInfraCatalogAdapter
    }

    @classmethod
    def get_adapter(cls, item_name: str):
        adapter_class = cls.adapters.get(item_name.lower())
        if not adapter_class:
            error(f"No adapter found for source: {item_name}")
            return None
        return adapter_class()