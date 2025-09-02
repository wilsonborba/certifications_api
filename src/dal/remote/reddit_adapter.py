# src/dal/remote/reddit_adapter.py
from __future__ import annotations

import time
from datetime import datetime, timezone
from typing import Any, Dict, Optional, Tuple, List
import requests

from src.core.settings import app_settings
from src.dal.remote.base import BaseAdapter
from src.domain.models.preview_model import EnumMode, PreviewModel

REDDIT_TOKEN_URL = "https://www.reddit.com/api/v1/access_token"
REDDIT_API_BASE = "https://oauth.reddit.com"

class RedditAdapter(BaseAdapter):
    item_name = "reddit"
    source_name = "apps"

    _token: Optional[str] = None
    _token_expiry: float = 0.0

    def get_preview(self) -> PreviewModel:
        return PreviewModel(
            mode=EnumMode.PLAYFUL,
            source_name=self.source_name,
            has_topic=True,
            item_name=self.item_name,
            item_img="https://res.cloudinary.com/dhncdmb2t/image/upload/v1756293205/reddit_logo_t93flf.png",
            updated_at=datetime.now(timezone.utc).isoformat()
        )
    
    def instructions(self) -> str:
        return (
            "You are given a Reddit post with comments. "
            "Your goal is to create fun, playful, and non-controversial quiz questions based on the content. "
            "Focus on who said what, general reactions, or funny, widely agreeable observations. "
            "Avoid questions that depend on subjective opinions or controversial interpretations. "
            "Keep the questions clear, light, and grounded in the content. "
            "All questions should be understandable and answerable by most users based on the provided context. "
            "Use a casual and fun tone. If needed, reference the post title or author to ground the question."
    )



    @property
    def _ua(self) -> str:
        return app_settings().REDDIT_USER_AGENT

    def _get_token(self) -> str:
        if self._token and time.time() < self._token_expiry - 30:
            return self._token
        s = app_settings()
        headers = {"User-Agent": self._ua}
        if s.REDDIT_CLIENT_SECRET:
            auth = (s.REDDIT_CLIENT_ID or "", s.REDDIT_CLIENT_SECRET)
            data = {"grant_type": "client_credentials", "scope": s.REDDIT_SCOPE}
        else:
            auth = (s.REDDIT_CLIENT_ID or "", "")
            data = {
                "grant_type": "https://oauth.reddit.com/grants/installed_client",
                "device_id": s.REDDIT_DEVICE_ID or "DO_NOT_TRACK_THIS_DEVICE",
                "scope": s.REDDIT_SCOPE,
            }
        resp = requests.post(REDDIT_TOKEN_URL, data=data, headers=headers, auth=auth, timeout=15)
        resp.raise_for_status()
        payload = resp.json()
        self._token = payload["access_token"]
        self._token_expiry = time.time() + int(payload.get("expires_in", 3600))
        return self._token

    def _get(self, path: str, params: Dict[str, Any] | None = None) -> Dict[str, Any]:
        token = self._get_token()
        headers = {"Authorization": f"Bearer {token}", "User-Agent": self._ua}
        url = f"{REDDIT_API_BASE}{path}"
        resp = requests.get(url, headers=headers, params=params or {}, timeout=20)
        resp.raise_for_status()
        return resp.json()

    def _page(self, path: str, *, limit: int, after: str | None = None, **extra: Any) -> tuple[list[dict], str | None]:
        limit = max(1, min(limit, 100))
        params = {"limit": limit, **extra}
        if after:
            params["after"] = after
        data = self._get(path, params=params).get("data", {}) or {}
        return data.get("children", []), data.get("after")


    def _kind_communities(self, *, limit: int, after: str | None) -> tuple[list[dict], str | None]:
        children, next_after = self._page("/subreddits/popular", limit=limit, after=after, sr_detail="true")
        trends: list[dict] = []
        for c in children:
            d = c.get("data", {}) or {}
            trends.append({
                "type": "subreddit",
                "topic_type": "subreddit",
                "input_identification": d.get("name"),  # e.g., t5_2qh33
                "name": d.get("display_name_prefixed") or d.get("display_name"),
                "display_name": d.get("display_name"),
                "title": d.get("title"),
                "subscribers": d.get("subscribers"),
                "url": f"https://www.reddit.com{d.get('url')}" if d.get("url") else None,
                "icon_img": d.get("community_icon") or d.get("icon_img"),
                "nsfw": d.get("over18"),
            })
        return trends, next_after

    def _kind_hot(self, *, limit: int, after: str | None) -> tuple[list[dict], str | None]:
        children, next_after = self._page("/r/all/hot", limit=limit, after=after)
        trends: list[dict] = []
        for c in children:
            d = c.get("data", {}) or {}
            trends.append({
                "type": "post",
                "topic_type": "post",
                "input_identification": d.get("name"),  # e.g., t3_abc123
                "title": d.get("title"),
                "subreddit": d.get("subreddit_name_prefixed"),
                "score": d.get("score"),
                "num_comments": d.get("num_comments"),
                "permalink": d.get("permalink"),
                "url": f"https://www.reddit.com{d.get('permalink')}" if d.get("permalink") else d.get("url"),
                "created_utc": d.get("created_utc"),
                "author": d.get("author"),
                "thumbnail": d.get("thumbnail") if (d.get("thumbnail") or "").startswith("http") else None,
                "nsfw": d.get("over_18"),
            })
        return trends, next_after

    def _kind_top(self, *, limit: int, after: str | None, time_window: str | None) -> tuple[list[dict], str | None]:
        t = (time_window or "day").lower()
        children, next_after = self._page("/r/all/top", limit=limit, after=after, t=t)
        trends: list[dict] = []
        for c in children:
            d = c.get("data", {}) or {}
            trends.append({
                "type": "post",
                "topic_type": "post",
                "input_identification": d.get("name"),  # e.g., t3_abc123
                "title": d.get("title"),
                "subreddit": d.get("subreddit_name_prefixed"),
                "score": d.get("score"),
                "num_comments": d.get("num_comments"),
                "permalink": d.get("permalink"),
                "url": f"https://www.reddit.com{d.get('permalink')}" if d.get("permalink") else d.get("url"),
                "created_utc": d.get("created_utc"),
                "author": d.get("author"),
                "thumbnail": d.get("thumbnail") if (d.get("thumbnail") or "").startswith("http") else None,
                "nsfw": d.get("over_18"),
                "time_window": t,
            })
        return trends, next_after

    
    def get_input(
        self,
        *,
        input_identification: str | None = None,
        topic_type: str | None = None,
        permalink_or_url: str | None = None,
        comments_limit: int = 20,
        depth: int = 1,
    ) -> Dict[str, Any]:
        """
        Fetch a full context for a topic previously returned by get_topics.
        - For posts (t3_*): returns post fields + top-level comments (up to comments_limit).
        - For subreddits (t5_*): returns 'about' + a small 'hot' listing.
        You may also pass a permalink/URL instead of input_identification.
        """
        assert comments_limit >= 0 and depth >= 0

        # Helper to GET with auth
        def _g(path: str, params: Dict[str, Any] | None = None) -> Dict[str, Any]:
            return self._get(path, params=params or {})

        # If permalink provided, try to resolve ID/type
        # e.g., /r/AskReddit/comments/abc123/some_title/
        if not input_identification and permalink_or_url:
            # Reddit supports /api/info by url:
            #   GET /api/info.json?url=https://www.reddit.com/r/.../comments/abc123/...
            # (We can pass the absolute URL or the permalink)
            url_param = permalink_or_url
            # ensure absolute URL (oauth host accepts full www.reddit.com URL)
            if url_param.startswith("/"):
                url_param = f"https://www.reddit.com{url_param}"
            info = _g("/api/info", params={"url": url_param})
            children = (info.get("data") or {}).get("children") or []
            if children:
                d = (children[0] or {}).get("data") or {}
                input_identification = d.get("name")  # t3_...
                topic_type = "post" if (input_identification or "").startswith("t3_") else None

        if not input_identification:
            return {"error": "missing input_identification and permalink_or_url"}

        # POSTS (t3_*)
        if input_identification.startswith("t3_") or topic_type == "post":
            # 1) Basic post info via /api/info?id=t3_xxx
            info = _g("/api/info", params={"id": input_identification})
            children = (info.get("data") or {}).get("children") or []
            if not children:
                return {"error": "post not found", "input_identification": input_identification}
            post = (children[0] or {}).get("data") or {}
            # 2) Comments via /comments/{id}.json
            base36 = input_identification.split("_", 1)[1]
            # parameters: depth, limit
            # Note: comments endpoint returns a 2-element array: [post, comments]
            comments_resp = self._get(f"/comments/{base36}.json", params={
                "limit": max(0, min(comments_limit, 200)),
                "depth": depth,
                "threaded": False,
                "sort": "top",
            })
            comments_list: list[dict] = []
            try:
                listing = comments_resp[1]  # second element is comments listing
                for c in (listing.get("data") or {}).get("children") or []:
                    if c.get("kind") != "t1":
                        continue
                    cd = (c.get("data") or {})
                    comments_list.append({
                        "id": cd.get("name"),            # t1_xxx
                        "author": cd.get("author"),
                        "body": cd.get("body"),
                        "score": cd.get("score"),
                        "created_utc": cd.get("created_utc"),
                        "replies_count": len(((cd.get("replies") or {}).get("data") or {}).get("children") or []) if isinstance(cd.get("replies"), dict) else 0,
                    })
            except Exception:
                pass

            return {
                # "topic_type": "post",
                "input_identification": input_identification,
                "input_data": {
                    "post": {
                    "id": post.get("name"),
                    "title": post.get("title"),
                    "selftext": post.get("selftext"),
                    "selftext_html": post.get("selftext_html"),
                    "author": post.get("author"),
                    "subreddit": post.get("subreddit_name_prefixed"),
                    "permalink": post.get("permalink"),
                    "url": f"https://www.reddit.com{post.get('permalink')}" if post.get("permalink") else post.get("url"),
                    "score": post.get("score"),
                    "num_comments": post.get("num_comments"),
                    "created_utc": post.get("created_utc"),
                    "over_18": post.get("over_18"),
                    "thumbnail": post.get("thumbnail") if (post.get("thumbnail") or "").startswith("http") else None,
                    "media": post.get("media") or post.get("secure_media"),
                    "preview": post.get("preview"),
                },
                "comments": comments_list,
                },
                "updated_at": datetime.now(timezone.utc).isoformat(),
            }

        # SUBREDDITS (t5_*)
        if input_identification.startswith("t5_") or topic_type == "subreddit":
            # /api/info?id=t5_xxx -> basic about data (but not all about fields)
            info = _g("/api/info", params={"id": input_identification})
            children = (info.get("data") or {}).get("children") or []
            if not children:
                return {"error": "subreddit not found", "input_identification": input_identification}
            sd = (children[0] or {}).get("data") or {}
            display = sd.get("display_name")
            # richer about:
            about = _g(f"/r/{display}/about")
            # a few hot posts:
            hot_children, _ = self._page(f"/r/{display}/hot", limit=10, after=None)
            hot = []
            for c in hot_children:
                d = (c.get("data") or {})
                hot.append({
                    "topic_type": "post",
                    "input_identification": d.get("name"),
                    "title": d.get("title"),
                    "permalink": d.get("permalink"),
                    "url": f"https://www.reddit.com{d.get('permalink')}" if d.get("permalink") else d.get("url"),
                    "author": d.get("author"),
                    "score": d.get("score"),
                    "num_comments": d.get("num_comments"),
                    "thumbnail": d.get("thumbnail") if (d.get("thumbnail") or "").startswith("http") else None,
                })
            return {
                # "topic_type": "subreddit",
                "input_identification": input_identification,
                "input_data": {
                "about": (about.get("data") or {}),
                "hot": hot
                },
                "updated_at": datetime.now(timezone.utc).isoformat(),
            }

        return {
            
            "input_identification": input_identification,

            "input_data": {
            },
            "updated_at": datetime.now(timezone.utc).isoformat(),
        }



    # ----- public: unified, numeric pagination -----
    def get_topics(
        self,
        *,
        page: int = 1,
        per_page: int = 30,
        time_window: str | None = None,   # adapter-specific; optional
        **_: Any
    ) -> Dict[str, Any]:
        """
        Build a mixed page (top + hot + communities) using numeric paging.
        We walk (page-1) cursor pages internally per kind, then fetch the page.
        """
        assert page >= 1 and per_page >= 1

        kinds = ["top", "hot", "communities"]
        base = per_page // len(kinds)
        remainder = per_page % len(kinds)
        per_kind_limits = [base + (1 if i < remainder else 0) for i in range(len(kinds))]

        # helper: returns (trends, has_more) for a given kind at numeric page
        def page_for_kind(kind: str, k_per_page: int) -> tuple[list[dict], bool]:
            if kind == "top":
                handler = lambda after: self._kind_top(limit=k_per_page, after=after, time_window=time_window)
            elif kind == "hot":
                handler = lambda after: self._kind_hot(limit=k_per_page, after=after)
            else:
                handler = lambda after: self._kind_communities(limit=k_per_page, after=after)

            cursor = None
            for _ in range(page - 1):
                _, cursor = handler(cursor)
                if not cursor:
                    break
            trends, next_cursor = handler(cursor)
            return trends, bool(next_cursor)

        merged: list[dict] = []
        any_has_more = False
        for idx, k in enumerate(kinds):
            t, has_more = page_for_kind(k, per_kind_limits[idx])
            merged.extend(t)
            any_has_more = any_has_more or has_more

        return {
            "topics": merged,
            "page": page,
            "per_page": per_page,
            "has_more": any_has_more,
            "updated_at": datetime.now(timezone.utc).isoformat(),
            "item_name": self.item_name,
            "source_name": self.source_name,
        }

    def generate_context(self, input_data: Dict[str, Any], amount_question: int = 10) -> str:
        post = input_data.get("post", {})
        comments = input_data.get("comments", [])
        title = post.get("title", "")
        subreddit = post.get("subreddit", "Unknown")
        post_body = post.get("selftext", "")
        permalink = post.get("permalink", "")

        context = f"Reddit post title: {title}\n"
        context += f"Subreddit: {subreddit}\n"
        context += f"Post body:\n{post_body.strip() or '[no text]'}\n\n"
        context += f"Top Comments:\n"

        for comment in comments[:10]:  # Limit comments for prompt size
            author = comment.get("author", "unknown")
            body = comment.get("body", "").strip()
            score = comment.get("score", 0)
            context += f"- {author} ({score} upvotes): {body}\n"

        context += self.context_output_structure(amount_question=amount_question)
        return context