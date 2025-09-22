# src/dal/remote/aws_infra_catalog_adapter.py
from __future__ import annotations

from typing import Any, Dict, List, Optional
from datetime import datetime, timezone
import requests
import boto3
from botocore.config import Config
from botocore.exceptions import BotoCoreError, ClientError, NoCredentialsError

from src.domain.models.indentifications_model import IdentificationsModel
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
                friendly['indentifications'] = IdentificationsModel(
                    input_identification=code,
                    title_identification=long_name.strip(),
                    link_identification=None,
                    img_link_identification=None,
                )
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
    
        # ---------- Public: fetch input/context for a single AWS service ----------
    def get_input(
        self,
        *,
        input_identification: str | None = None,
        include_regions: bool = True,
        max_regions: int | None = None,
        **_: Any
    ) -> Dict[str, Any]:
        """
        Resolve an AWS service by code or friendly name and return structured context.

        Contract (example):
          {
            "identifications": IdentificationsModel(...),
            "input_data": {
              "meta": {
                "service_code": "athena",
                "service_name": "Amazon Athena",
                "external_url": "https://aws.amazon.com/athena/",
                "pricing_offer_code": "AmazonAthena",
                "pricing_index_hint": "offers/v1.0/aws/AmazonAthena/current/index.json"
              },
              "regions": ["us-east-1", "us-west-2", ...]  # optional
            },
            "updated_at": "<iso>"
          }
        """
        now_iso = datetime.now(timezone.utc).isoformat()
        raw = (input_identification or "").strip()

        # ---------- 1) Normalize the query ----------
        # Allow either a code ("s3") or a friendly name ("Amazon S3").
        # We'll try to resolve both the canonical code and the nice display name.
        query_code = (raw or "").lower()
        query_name = raw

        # Empty input: return a helpful error-shaped payload
        if not raw:
            out: Dict[str, Any] = {
                "input_data": {"error": "missing_input_identification"},
                "updated_at": now_iso,
            }
            out["identifications"] = IdentificationsModel(
                input_identification=None,
                title_identification=None,
                link_identification=None,
                img_link_identification=None,
            )
            return out

        # ---------- 2) Collect known service codes ----------
        # Prefer SSM public tree; fallback to pricing offer map for breadth.
        try:
            # Values under this path are the *service codes*, e.g. "athena", "s3", ...
            ssm_services = self._ssm_list_parameters(
                "/aws/service/global-infrastructure/services",
                recursive=False,
                max_results=10
            )
            service_codes = [p.get("Value") for p in ssm_services if p.get("Value")]
        except Exception:
            service_codes = []

        pricing_name_map = self._pricing_index_names()  # {"athena": "Amazon Athena", "AmazonAthena": "Amazon Athena", ...}
        # From pricing map, grab plausible codes (keys that look like lowercase hyphen/alpha are probably codes)
        pricing_guessed_codes = [k for k in pricing_name_map.keys() if k.islower() and "-" in k or k.isalpha()]
        known_codes = set(service_codes or []) | set(pricing_guessed_codes)

        # ---------- 3) Try to resolve "raw" to (service_code, service_name) ----------
        resolved_code: Optional[str] = None
        resolved_name: Optional[str] = None

        # Helper: fetch friendly name from SSM longName/name if possible
        def friendly_from_ssm(code: str) -> Optional[str]:
            long_name = self._ssm_get_parameter_value(f"/aws/service/global-infrastructure/services/{code}/longName")
            if not long_name:
                long_name = self._ssm_get_parameter_value(f"/aws/service/global-infrastructure/services/{code}/name")
            return (long_name or None)

        # Case A: user passed a code we recognize
        if query_code in known_codes:
            resolved_code = query_code
            resolved_name = friendly_from_ssm(query_code) or pricing_name_map.get(query_code) or query_name

        # Case B: user passed a friendly name; try to map to code
        if not resolved_code:
            # First: scan SSM tree for a direct friendly name match
            # (We only have the code list cheaply; we’ll attempt lookups to find a match)
            for code in sorted(known_codes):
                nm = friendly_from_ssm(code) or pricing_name_map.get(code)
                if nm and nm.lower() == query_name.lower():
                    resolved_code, resolved_name = code, nm
                    break

        # Case C: fuzzy fallback via Pricing names (case-insensitive contains)
        if not resolved_code:
            lowered = query_name.lower()
            # First pass: exact case-insensitive match on any pricing entry
            for key, disp in pricing_name_map.items():
                if disp and disp.lower() == lowered and key in known_codes:
                    resolved_code = key
                    resolved_name = disp
                    break
            # Second pass: substring match against pricing names
            if not resolved_code:
                for key, disp in pricing_name_map.items():
                    if disp and lowered in disp.lower() and key in known_codes:
                        resolved_code = key
                        resolved_name = disp
                        break

        # If still unresolved, we’ve got nothing solid
        if not resolved_code:
            out: Dict[str, Any] = {
                "input_data": {"error": "not_found"},
                "updated_at": now_iso,
            }
            out["identifications"] = IdentificationsModel(
                input_identification=raw,
                title_identification=None,
                link_identification=None,
                img_link_identification=None,
            )
            return out

        # Finalize friendly name
        resolved_name = resolved_name or friendly_from_ssm(resolved_code) or pricing_name_map.get(resolved_code) or resolved_code

        # ---------- 4) Enrich: regions where this service is available ----------
        regions: List[str] = []
        if include_regions:
            # The public SSM tree exposes a couple of shapes; we’ll try the most common:
            #   /aws/service/global-infrastructure/region-service-maps/<service_code>/regions
            # If that fails, we’ll try scanning all regions and checking a per-region flag.
            try:
                base = f"/aws/service/global-infrastructure/region-service-maps/{resolved_code}/regions"
                ps = self._ssm_list_parameters(base, recursive=False, max_results=50)
                regions = [p.get("Value") for p in ps if p.get("Value")]
            except Exception:
                regions = []

            if not regions:
                try:
                    # Fallback: list all regions then probe availability flags
                    all_regs = self._ssm_list_parameters(
                        "/aws/service/global-infrastructure/regions",
                        recursive=False,
                        max_results=50,
                    )
                    candidate_regs = [p.get("Value") for p in all_regs if p.get("Value")]
                except Exception:
                    candidate_regs = []

                probed: List[str] = []
                for r in candidate_regs:
                    # Several shapes exist; try a few without failing the whole request
                    # (Any read failure just means "unknown" for that region.)
                    ok = False
                    for path in (
                        f"/aws/service/global-infrastructure/regions/{r}/services/{resolved_code}/available",
                        f"/aws/service/global-infrastructure/region-services/{r}/services/{resolved_code}/available",
                    ):
                        val = self._ssm_get_parameter_value(path)
                        if val and str(val).strip().lower() in ("true", "1", "yes"):
                            ok = True
                            break
                    if ok:
                        probed.append(r)

                if probed:
                    regions = sorted(probed)

            if max_regions and regions:
                regions = regions[:max_regions]

        # ---------- 5) Pricing offer metadata & external URL guess ----------
        # pricing_name_map has both display names and sometimes keys like "AmazonAthena".
        # Try to find an offerCode-ish key that maps to our display name.
        pricing_offer_code = None
        for key, disp in pricing_name_map.items():
            if disp and disp.strip().lower() == resolved_name.strip().lower() and key != resolved_code and key[0].isupper():
                pricing_offer_code = key
                break

        pricing_index_hint = None
        if pricing_offer_code:
            pricing_index_hint = f"offers/v1.0/aws/{pricing_offer_code}/current/index.json"

        # External (marketing) URL: most services work with this pattern.
        external_url = f"https://aws.amazon.com/{resolved_code}/"

        # ---------- 6) Shape the response ----------
        input_data: Dict[str, Any] = {
            "meta": {
                "service_code": resolved_code,
                "service_name": resolved_name,
                "external_url": external_url,
            }
        }
        if pricing_offer_code:
            input_data["meta"]["pricing_offer_code"] = pricing_offer_code
        if pricing_index_hint:
            input_data["meta"]["pricing_index_hint"] = pricing_index_hint
        if regions:
            input_data["regions"] = regions

        out: Dict[str, Any] = {
            "input_data": input_data,
            "updated_at": now_iso,
        }
        out["identifications"] = IdentificationsModel(
            input_identification=resolved_code,
            title_identification=resolved_name,
            link_identification=external_url,
            img_link_identification=None,
        )
        return out

        
