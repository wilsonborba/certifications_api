# src/dal/remote/reddit_adapter.py
from __future__ import annotations

import time
from datetime import datetime, timezone
from typing import Any, Dict, Optional, Tuple, List
import requests

from src.domain.models.indentifications_model import IdentificationsModel
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

            t_community = {
                "type": "subreddit",
                "topic_type": "subreddit",
                
                "name": d.get("display_name_prefixed") or d.get("display_name"),
                "display_name": d.get("display_name"),
                "title": d.get("title"),
                "subscribers": d.get("subscribers"),
                "url": f"https://www.reddit.com{d.get('url')}" if d.get("url") else None,
                "icon_img": d.get("community_icon") or d.get("icon_img"),
                "nsfw": d.get("over18"),
            }

            t_community['identifications'] = IdentificationsModel(
                input_identification=d.get("name"),  # e.g., t5_2qh33
                title_identification=d.get("title"),
                link_identification=t_community["url"],
                img_link_identification=t_community["icon_img"],
            )

            trends.append(t_community)
        return trends, next_after

    def _kind_hot(self, *, limit: int, after: str | None) -> tuple[list[dict], str | None]:
        children, next_after = self._page("/r/all/hot", limit=limit, after=after)
        trends: list[dict] = []
        for c in children:
            d = c.get("data", {}) or {}

            t_hot = {
                "type": "post",
                "topic_type": "post",
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
            }

            t_hot['identifications'] = IdentificationsModel(
                input_identification=d.get("name"),  # e.g., t3_abc123
                title_identification=d.get("title"),
                link_identification=t_hot["url"],
                img_link_identification=t_hot["thumbnail"],
            )

            trends.append(t_hot)
        return trends, next_after

    def _kind_top(self, *, limit: int, after: str | None, time_window: str | None) -> tuple[list[dict], str | None]:
        t = (time_window or "day").lower()
        children, next_after = self._page("/r/all/top", limit=limit, after=after, t=t)
        trends: list[dict] = []
        for c in children:
            d = c.get("data", {}) or {}
            t_top = {
                "type": "post",
                "topic_type": "post",

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
            }

            t_top['identifications'] = IdentificationsModel(
                input_identification=d.get("name"),  # e.g., t3_abc123
                title_identification=d.get("title"),
                link_identification=t_top["url"],
                img_link_identification=t_top["thumbnail"],
            )

            trends.append(t_top)
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

            input_ = {
                # "topic_type": "post",
                
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

            input_['identifications'] = IdentificationsModel(
                input_identification=input_identification,  # e.g., t3_abc123
                title_identification=post.get("title"),
                link_identification=input_.get("input_data", {}).get("post", {}).get("url", None),
                img_link_identification=input_.get("input_data", {}).get("post", {}).get("thumbnail", None),
            )

            return input_

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

                t_hot_2 = {
                    "topic_type": "post",

                    "title": d.get("title"),
                    "permalink": d.get("permalink"),
                    "url": f"https://www.reddit.com{d.get('permalink')}" if d.get("permalink") else d.get("url"),
                    "author": d.get("author"),
                    "score": d.get("score"),
                    "num_comments": d.get("num_comments"),
                    "thumbnail": d.get("thumbnail") if (d.get("thumbnail") or "").startswith("http") else None,
                }

                t_hot_2['identifications'] = IdentificationsModel(
                    input_identification=d.get("name"),  # e.g., t3_abc123
                    title_identification=d.get("title"),
                    link_identification=t_hot_2["url"],
                    img_link_identification=t_hot_2["thumbnail"],
                )

                hot.append(t_hot_2)
           
            input_2 = {
                # "topic_type": "subreddit",
                
                "input_data": {
                    
                "about": (about.get("data") or {}),
                "hot": hot
                },
                "updated_at": datetime.now(timezone.utc).isoformat(),
            }

            input_2['identifications'] = IdentificationsModel(
                input_identification=input_identification,  # e.g., t3_abc123
                title_identification=None,
                link_identification=None,
                img_link_identification=None,
            )

            return input_2

        input_3 = {
            
 
            "input_data": {
                
            },
            "updated_at": datetime.now(timezone.utc).isoformat(),
        }

        input_3['identifications'] = IdentificationsModel(
            input_identification=input_identification,
            title_identification=None,
            link_identification=None,
            img_link_identification=None,
        )



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
    
    def search(
        self,
        q: str,
        *,
        page: int = 1,
        per_page: int = 20,
        mode: str = "fulltext",           # "fulltext" | "substring" | "fuzzy"
        time_window: str | None = None,   # "hour","day","week","month","year","all"
        fill_page: bool = True,           # tenta completar per_page após filtrar
        max_extra_pages: int = 2,         # limite de páginas extras para fill
    ) -> dict:
        assert isinstance(q, str) and q.strip(), "q vazio"
        assert page >= 1 and per_page >= 1

        # 1) Caminhar até a página pedida usando 'after'
        cursor = None
        for _ in range(page - 1):
            _, cursor = self._search_page(q=q, limit=per_page, after=cursor, sort="relevance", t=time_window)
            if not cursor:
                return {
                    "item_name": self.item_name,
                    "source_name": self.source_name,
                    "page": page,
                    "per_page": per_page,
                    "has_more": False,
                    "updated_at": datetime.now(timezone.utc).isoformat(),
                    "topics": [],
                }

        children, next_after = self._search_page(q=q, limit=per_page, after=cursor, sort="relevance", t=time_window)
        items = [self._normalize_search_child(c) for c in children]
        items = [it for it in items if it is not None]  # remove inválidos

        # 2) Modos de busca
        if mode in ("substring", "fuzzy"):
            # enriquecer com corpo p/ filtrar localmente
            items = self._enrich_with_body(items)
            items = self._apply_substring(items, q) if mode == "substring" else self._apply_fuzzy(items, q)

            # opcionalmente tentar completar per_page pegando páginas seguintes
            if fill_page and len(items) < per_page and next_after:
                items, next_after = self._fill_current_page(
                    base_items=items,
                    already_seen_ids={it["identifications"].input_identification for it in items},
                    q=q,
                    mode=mode,
                    time_window=time_window,
                    after=next_after,
                    per_page=per_page,
                    max_extra_pages=max_extra_pages,
                )

        # 3) Ordenação leve por score e recência
        items.sort(key=lambda x: ((x.get("score") or 0), self._safe_ts(x.get("created_at"))), reverse=True)

        return {
            "item_name": self.item_name,
            "source_name": self.source_name,
            "page": page,
            "per_page": per_page,
            "has_more": bool(next_after),
            "updated_at": datetime.now(timezone.utc).isoformat(),
            "topics": items[:per_page],   # drop-in para o grid atual
        }

    # ---------- HELPERS (SEARCH) ----------
    def _search_page(self, *, q: str, limit: int, after: str | None, sort: str, t: str | None):
        params = {"q": q, "limit": max(1, min(limit, 100)), "sort": sort}
        if t:
            params["t"] = t
        if after:                                 # <-- FALTAVA ISSO
            params["after"] = after
        data = self._get("/search", params=params).get("data", {}) or {}
        return (data.get("children", []) or [], data.get("after"))

    def _normalize_search_child(self, child: dict) -> dict | None:
        from src.domain.models.indentifications_model import IdentificationsModel  # usa seu DataClass

        d = (child or {}).get("data") or {}
        rid = d.get("name")            # t3_xxx
        title = (d.get("title") or "").strip()
        permalink = d.get("permalink")
        link = (f"https://www.reddit.com{permalink}" if permalink else d.get("url") or "").strip()

        # front exige id + title + link não vazios; se faltar, descarta
        if not rid or not title or not link:
            return None

        thumb = d.get("thumbnail")
        img = thumb if isinstance(thumb, str) and thumb.startswith("http") else None

        ident = IdentificationsModel(
            input_identification=rid,
            title_identification=title,
            link_identification=link,
            img_link_identification=img,
        )

        return {
            "type": "post",
            "title": title,
            "url": link,
            "score": d.get("score"),
            "created_at": self._utc_to_iso(d.get("created_utc")),
            "identifications": ident,  # <<-- DataClass, como no get_topics
        }

    def _enrich_with_body(self, items: list[dict]) -> list[dict]:
        ids = [it["identifications"].input_identification for it in items]
        if not ids:
            return items

        chunk = 50
        id_chunks = [ids[i:i+chunk] for i in range(0, len(ids), chunk)]
        by_id = {}
        for ch in id_chunks:
            data = self._get("/api/info", params={"id": ",".join(ch)})
            for c in ((data.get("data") or {}).get("children") or []):
                d = (c or {}).get("data") or {}
                by_id[d.get("name")] = d

        enriched = []
        for it in items:
            rid = it["identifications"].input_identification
            d = by_id.get(rid, {})
            body = (d.get("selftext") or "").strip()
            it2 = dict(it)
            it2["body"] = body
            enriched.append(it2)
        return enriched

    def _apply_substring(self, items: list[dict], q: str) -> list[dict]:
        qn = (q or "").casefold()
        out = []
        for it in items:
            title = (it.get("title") or "")
            body = (it.get("body") or "")
            if (qn in title.casefold()) or (qn in body.casefold()):
                it2 = dict(it)
                it2["highlights"] = self._make_highlights(title, body, q)
                out.append(it2)
        return out

    def _apply_fuzzy(self, items: list[dict], q: str, *, threshold: float = 0.78) -> list[dict]:
        qn = (q or "").casefold()
        out = []
        for it in items:
            title = (it.get("title") or "").casefold()
            body  = (it.get("body") or "").casefold()
            st = self._simple_fuzzy_score(title, qn)
            sb = self._simple_fuzzy_score(body,  qn)
            if max(st, sb) >= threshold:
                it2 = dict(it)
                it2["highlights"] = self._make_highlights(it.get("title") or "", it.get("body") or "", q)
                out.append(it2)
        return out

    def _fill_current_page(
        self,
        *,
        base_items: list[dict],
        already_seen_ids: set[str],
        q: str,
        mode: str,
        time_window: str | None,
        after: str | None,
        per_page: int,
        max_extra_pages: int,
    ) -> tuple[list[dict], str | None]:
        """
        Busca mais 1..N páginas para completar per_page após filtro, sem estourar custo.
        """
        items = list(base_items)
        cursor = after
        extra_scanned = 0

        while cursor and len(items) < per_page and extra_scanned < max_extra_pages:
            children, next_after = self._search_page(q=q, limit=per_page, after=cursor, sort="relevance", t=time_window)
            cand = [self._normalize_search_child(c) for c in children]
            cand = [it for it in cand if it is not None and it["identifications"].input_identification not in already_seen_ids]

            if mode in ("substring", "fuzzy"):
                cand = self._enrich_with_body(cand)
                cand = self._apply_substring(cand, q) if mode == "substring" else self._apply_fuzzy(cand, q)

            for it in cand:
                if len(items) >= per_page:
                    break
                rid = it["identifications"].input_identification
                if rid in already_seen_ids:
                    continue
                already_seen_ids.add(rid)
                items.append(it)

            cursor = next_after
            extra_scanned += 1

        return items, cursor

    # ---------- utilitários usados ----------
    def _make_highlights(self, title: str, body: str, q: str, ctx: int = 40) -> dict:
        def _hl(txt: str, q_: str) -> str | None:
            t = txt or ""
            i = t.casefold().find(q_.casefold())
            if i == -1: return None
            start = max(0, i - ctx); end = min(len(t), i + len(q_) + ctx)
            return t[start:end]
        return {
            "title": _hl(title, q) or (title[:80] if title else None),
            "snippet": _hl(body, q)  or ((body or "")[:160] or None),
        }

    def _simple_fuzzy_score(self, text: str, query: str) -> float:
        if not text or not query: return 0.0
        if query in text: return min(1.0, 0.6 + len(query) / max(len(text), len(query)))
        pref = 1.0 if text.startswith(query) else 0.0
        suff = 1.0 if text.endswith(query) else 0.0
        best = 0; qlen = len(query)
        for w in range(min(qlen, 8), 1, -1):
            if any(query[i:i+w] in text for i in range(0, qlen - w + 1)): best = w; break
        base = best / qlen
        return min(1.0, 0.15 + 0.35 * base + 0.25 * pref + 0.25 * suff)

    def _utc_to_iso(self, ts: float | int | None) -> str | None:
        try:
            if ts is None: return None
            return datetime.fromtimestamp(float(ts), tz=timezone.utc).isoformat()
        except Exception:
            return None

    def _safe_ts(self, iso: str | None) -> float:
        try:
            return datetime.fromisoformat(iso).timestamp() if iso else 0.0
        except Exception:
            return 0.0

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