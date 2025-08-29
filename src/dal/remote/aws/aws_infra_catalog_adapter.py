# src/dal/remote/aws_infra_catalog_adapter.py
from __future__ import annotations

from typing import Any, Dict, List, Optional
from datetime import datetime, timezone
import requests
import boto3
from botocore.config import Config
from botocore.exceptions import BotoCoreError, ClientError, NoCredentialsError

from src.dal.remote.base import BaseAdapter
from src.domain.models.preview_model import PreviewModel, EnumMode
from src.core.logs import debug, error
from src.core.settings import app_settings

PRICING_INDEX = "https://pricing.us-east-1.amazonaws.com/offers/v1.0/aws/index.json"

class AwsInfraCatalogAdapter(BaseAdapter):
    item_name = "aws_infra_catalog"
    source_name = "apps"

    def __init__(self, *, aws_region: str | None = None) -> None:
        s = app_settings()  # <- your pydantic-backed settings
        region = aws_region or s.AWS_REGION  # default comes from settings

        # If you also add AWS_SESSION_TOKEN to Settings, we’ll pick it up automatically.
        session_token: Optional[str] = getattr(s, "AWS_SESSION_TOKEN", None)

        # Build an isolated boto3 session with the project-specific creds.
        session = boto3.Session(
            aws_access_key_id=s.AWS_ACCESS_KEY_ID,
            aws_secret_access_key=s.AWS_SECRET_ACCESS_KEY,
            aws_session_token=session_token,
            region_name=region,
        )

        self._ssm = session.client(
            "ssm",
            config=Config(retries={"max_attempts": 5, "mode": "standard"})
        )

    def get_preview(self) -> PreviewModel:
        return PreviewModel(
            mode=EnumMode.SERIOUS,
            source_name=self.source_name,
            has_topic=True,
            item_name=self.item_name,
            item_img="https://res.cloudinary.com/dhncdmb2t/image/upload/v1756380014/aws_odvewo.png",
            updated_at=datetime.now(timezone.utc).isoformat(),
        )

    # ---------- SSM helpers ----------
    def _ssm_list_parameters(self, path: str, *, recursive: bool = False, max_results: int = 10) -> List[Dict[str, Any]]:
        out: List[Dict[str, Any]] = []
        token: Optional[str] = None
        while True:
            kwargs = dict(Path=path, Recursive=recursive, WithDecryption=False, MaxResults=max_results)
            if token:
                kwargs["NextToken"] = token
            resp = self._ssm.get_parameters_by_path(**kwargs)
            out.extend(resp.get("Parameters", []))
            token = resp.get("NextToken")
            if not token:
                break
        return out

    def _ssm_get_parameter_value(self, name: str) -> Optional[str]:
        try:
            r = self._ssm.get_parameter(Name=name, WithDecryption=False)
            return r.get("Parameter", {}).get("Value")
        except (BotoCoreError, ClientError) as e:
            debug(f"SSM get_parameter failed for {name}: {e}")
            return None

    # ---------- Pricing index fallback ----------
    def _pricing_index_names(self) -> Dict[str, str]:
        try:
            r = requests.get(PRICING_INDEX, timeout=20)
            r.raise_for_status()
            data = r.json()
        except Exception as e:
            debug(f"Pricing index fetch failed: {e}")
            return {}
        out: Dict[str, str] = {}
        for svc_key, meta in (data.get("offers") or {}).items():
            display = svc_key.replace("-", " ").replace("_", " ")
            offer = meta.get("offerCode") or ""
            out[svc_key] = display
            if offer:
                out[offer] = display
        return out

    # ---------- Build catalog from SSM ----------
    def _collect_service_names(self) -> List[str]:
        try:
            # This path returns *parameters whose Value is the service code* (e.g., 'athena', 's3', ...)
            params = self._ssm_list_parameters("/aws/service/global-infrastructure/services", recursive=False, max_results=10)
        except (NoCredentialsError, BotoCoreError, ClientError) as e:
            error(f"SSM list services failed: {e}")
            params = []

        service_codes = [p.get("Value") for p in params if p.get("Value")]
        # Fallback: if some environments return empty at the shallow level, walk recursively once.
        if not service_codes:
            try:
                deep = self._ssm_list_parameters("/aws/service/global-infrastructure/services", recursive=True, max_results=10)
                seen = set()
                for pp in deep:
                    name = pp.get("Name", "")
                    # extract code between '/services/' and next '/'
                    if "/services/" in name:
                        code = name.split("/services/")[-1].split("/")[0]
                        if code:
                            seen.add(code)
                service_codes = sorted(seen)
            except (NoCredentialsError, BotoCoreError, ClientError) as e:
                error(f"SSM deep list failed: {e}")
                service_codes = []

        # Resolve friendly names via /.../services/<code>/longName (or /name)
        pricing_fallback = None
        friendly: List[str] = []
        for code in service_codes:
            long_name = self._ssm_get_parameter_value(f"/aws/service/global-infrastructure/services/{code}/longName")
            if not long_name:
                long_name = self._ssm_get_parameter_value(f"/aws/service/global-infrastructure/services/{code}/name")
            if long_name:
                friendly.append(long_name.strip())
            else:
                if pricing_fallback is None:
                    pricing_fallback = self._pricing_index_names()
                friendly.append((pricing_fallback.get(code) or code).strip())

        # dedupe, keep order
        seen = set()
        result: List[str] = []
        for n in friendly:
            if n and n not in seen:
                seen.add(n)
                result.append(n)
        return result

    # ---------- Public: unified Topics ----------
    def get_topics(self, *, page: int = 1, per_page: int = 60, **_: Any) -> Dict[str, Any]:
        assert page >= 1 and per_page >= 1
        services = self._collect_service_names()
        start = (page - 1) * per_page
        end = start + per_page
        slice_ = services[start:end]

        topics = [{"service": name} for name in slice_]
        has_more = end < len(services)

        return {
            "topics": topics,
            "page": page,
            "per_page": per_page,
            "has_more": has_more,
            "fetched_at": datetime.now(timezone.utc).isoformat(),
            "item_name": self.item_name,
            "source_name": self.source_name,
        }
