# src/dal/remote/meetup_adapter.py
from __future__ import annotations

from typing import Any, Dict, List, Optional, Tuple
from datetime import datetime, timezone
import re
import html
import requests

from src.dal.remote.base import BaseAdapter
from src.domain.models.preview_model import PreviewModel, EnumMode
from src.core.settings import app_settings

MEETUP_API_BASE = "https://api.meetup.com"
FIND_UPCOMING = f"{MEETUP_API_BASE}/find/upcoming_events"   # v3 REST

def _iso(dt_str: Optional[str]) -> Optional[str]:
    if not dt_str:
        return None
    # Meetup v3 returns ISO-like strings or RFC3339; trust and pass through.
    try:
        # normalize to aware UTC if no tz present
        dt = datetime.fromisoformat(dt_str.replace("Z", "+00:00"))
        if dt.tzinfo is None:
            dt = dt.replace(tzinfo=timezone.utc)
        return dt.astimezone(timezone.utc).isoformat()
    except Exception:
        return dt_str

def _excerpt(text: Optional[str], n: int = 300) -> Optional[str]:
    if not text:
        return None
    un = html.unescape(text)
    # strip basic HTML tags if present
    un = re.sub(r"<[^>]+>", " ", un)
    un = re.sub(r"\s+", " ", un).strip()
    return un[:n]

class MeetupAdapter(BaseAdapter):
    """
    Topic = real-world events near a location in a date window.
    Source fields: title, description excerpt, start time, RSVP counts, group, venue/city, topics/tags, URL.
    Pagination: numeric (page/per_page) using Meetup 'page' and slicing if needed.
    Auth: OAuth2 Bearer token (put in your Settings, e.g., MEETUP_ACCESS_TOKEN).
    """
    item_name = "meetup"
    source_name = "apps"

    def __init__(
        self,
        *,
        access_token: Optional[str] = None,      # if None, read from settings when you add it
    ) -> None:
        self._token = app_settings().MEETUP_ACCESS_TOKEN


    # ---------- Preview ----------
    def get_preview(self) -> PreviewModel:
        return PreviewModel(
            mode=EnumMode.PLAYFUL,  # also fits BOTH; Playful quizzes like “What’s happening near you?”
            source_name=self.source_name,
            has_topic=True,
            item_name=self.item_name,
            item_img="https://res.cloudinary.com/dhncdmb2t/image/upload/v1756531000/meetup_app_icon_jf0y5f.png",
            updated_at=datetime.now(timezone.utc).isoformat(),
        )

    # ---------- HTTP ----------
    def _headers(self) -> Dict[str, str]:
        h = {"User-Agent": "quiz-certify/1.0"}
        if self._token:
            h["Authorization"] = f"Bearer {self._token}"
        return h

    def _get(self, url: str, params: Dict[str, Any]) -> Dict[str, Any]:
        r = requests.get(url, params=params, headers=self._headers(), timeout=20)
        r.raise_for_status()
        return r.json()

    # ---------- REST v3: find upcoming events ----------
    def _find_upcoming_events(
        self,
        *,
        page: int,
        per_page: int,
        lat: Optional[float],
        lon: Optional[float],
        city: Optional[str],
        radius: Optional[str],
        start_date_range: Optional[str],
        end_date_range: Optional[str],
        keywords: Optional[List[str]],
        topic_category_ids: Optional[List[int]],
    ) -> Tuple[List[Dict[str, Any]], bool]:
        """
        Calls /find/upcoming_events with sensible filters.
        Returns (events, has_more)
        """
        # Meetup 'page' (page size) is per request; we emulate numeric pages by offset slicing.
        # We’ll fetch (per_page) and then compute has_more by probing the total slice length if needed.
        params: Dict[str, Any] = {
            "page": max(1, per_page),
        }
        # Location: prefer lat/lon; fallback to 'text' city search
        if lat is not None and lon is not None:
            params["lat"] = lat
            params["lon"] = lon
        elif city:
            params["text"] = city

        if radius:
            params["radius"] = radius  # e.g., "smart", "global", or miles/km per API

        if start_date_range:
            params["start_date_range"] = start_date_range  # ISO 8601
        if end_date_range:
            params["end_date_range"] = end_date_range

        # Keyword/topic filters
        if keywords:
            # Meetup uses 'text' for keyword search; join multiple with spaces
            params["text"] = " ".join(keywords) if not params.get("text") else f"{params['text']} " + " ".join(keywords)
        if topic_category_ids:
            # Some clients pass 'topic_category' or fields; keep optional
            params["topic_category"] = ",".join(str(i) for i in topic_category_ids)

        data = self._get(FIND_UPCOMING, params)
        raw_events = (data.get("events") or []) if isinstance(data, dict) else []
        # Meetup returns exactly up to 'page' items; to support numeric pages, you call this per page.
        # Here we just indicate has_more if we received page-sized chunk.
        has_more = len(raw_events) >= per_page

        return raw_events, has_more

    # ---------- Normalize ----------
    def _normalize_events(self, events: List[Dict[str, Any]]) -> List[Dict[str, Any]]:
        topics: List[Dict[str, Any]] = []
        for ev in events:
            # Common fields safely extracted
            name = ev.get("name")
            link = ev.get("link")
            time_iso = None
            if ev.get("local_date") and ev.get("local_time"):
                time_iso = _iso(f"{ev['local_date']}T{ev['local_time']}:00")
            elif ev.get("time"):  # ms since epoch sometimes
                try:
                    ts = int(ev["time"]) / 1000.0
                    time_iso = datetime.fromtimestamp(ts, tz=timezone.utc).isoformat()
                except Exception:
                    time_iso = None

            group = (ev.get("group") or {})
            venue = (ev.get("venue") or {})
            desc = ev.get("description")

            topics.append({
                "type": "event",
                "title": name,
                "url": link,
                "time_iso": time_iso,
                "rsvp_yes": (ev.get("yes_rsvp_count") or 0),
                "rsvp_waitlist": (ev.get("waitlist_count") or 0),
                "group": {
                    "name": group.get("name"),
                    "urlname": group.get("urlname"),
                    "id": group.get("id"),
                },
                "venue": {
                    "name": venue.get("name"),
                    "city": venue.get("city") or (ev.get("venue_city") if "venue_city" in ev else None),
                    "address": venue.get("address_1"),
                    "lat": venue.get("lat"),
                    "lon": venue.get("lon"),
                },
                "tags": [t.get("name") for t in (ev.get("topics") or []) if isinstance(t, dict) and t.get("name")],
                "excerpt": _excerpt(desc),
            })
        return topics

    # ---------- Public: unified Topics ----------
    def get_topics(
        self,
        *,
        page: int = 1,
        per_page: int = 30,
        # location filters
        lat: Optional[float] = None,
        lon: Optional[float] = None,
        city: Optional[str] = None,
        radius: Optional[str] = "smart",  # "smart", "global", or numeric miles/km per Meetup
        # time window (ISO 8601; e.g., "2025-08-28T00:00:00Z")
        start_date_range: Optional[str] = None,
        end_date_range: Optional[str] = None,
        # seed topics / keywords (e.g., ["AI/ML", "Web Dev", "Climate"])
        seed_topics: Optional[List[str]] = None,
        topic_category_ids: Optional[List[int]] = None,
        **_: Any
    ) -> Dict[str, Any]:
        assert page >= 1 and per_page >= 1

        # Meetup REST v3 is page-sized; emulate numeric pages by passing per_page and using Meetup's pagination
        # You can also keep a stable cursor outside; for MVP we go numeric-direct (page => a new request).
        raw, has_more = self._find_upcoming_events(
            page=page,
            per_page=per_page,
            lat=lat, lon=lon, city=city,
            radius=radius,
            start_date_range=start_date_range,
            end_date_range=end_date_range,
            keywords=seed_topics,
            topic_category_ids=topic_category_ids,
        )
        topics = self._normalize_events(raw)

        return {
            "topics": topics,
            "page": page,
            "per_page": per_page,
            "has_more": bool(has_more and len(topics) >= per_page),
            "fetched_at": datetime.now(timezone.utc).isoformat(),
            "item_name": self.item_name,
            "source_name": self.source_name,
        }
