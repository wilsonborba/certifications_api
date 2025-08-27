
from datetime import datetime
from src.dal.remote.base import BaseAdapter
from src.domain.models.preview_model import EnumMode, PreviewModel


class RedditAdapter(BaseAdapter):
    
    item_name = "reddit"
    source_name = "apps"

    def get_preview(self) -> PreviewModel:
        # Mock implementation, replace with actual Reddit API calls
        return PreviewModel(
            mode=EnumMode.PLAYFUL,
            source_name=self.source_name,
            has_topic=True,
            item_name=self.item_name,
            item_img="https://res.cloudinary.com/dhncdmb2t/image/upload/v1756293205/reddit_logo_t93flf.png",
            updated_at=datetime.now().isoformat()
        )