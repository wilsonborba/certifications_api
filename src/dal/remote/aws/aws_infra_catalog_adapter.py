# src/dal/remote/aws_infra_catalog_adapter.py
from __future__ import annotations

import json
from typing import Any, Dict, List, Optional, Tuple
from datetime import datetime, timezone
import requests
import boto3
from botocore.config import Config
from botocore.exceptions import BotoCoreError, ClientError, NoCredentialsError, DataNotFoundError
from botocore.session import Session as BotocoreSession

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
        s = app_settings()
        region = aws_region or s.AWS_REGION
        session_token: Optional[str] = getattr(s, "AWS_SESSION_TOKEN", None)

        # Project-scoped boto3 session
        self._boto3 = boto3.Session(
            aws_access_key_id=s.AWS_ACCESS_KEY_ID,
            aws_secret_access_key=s.AWS_SECRET_ACCESS_KEY,
            aws_session_token=session_token,
            region_name=region,
        )

        self._ssm = self._boto3.client(
            "ssm",
            config=Config(retries={"max_attempts": 5, "mode": "standard"})
        )

        # A plain botocore session gives us service models & endpoint resolver
        self._botocore = BotocoreSession()

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

    # ---------- Collect services (code + friendly name) ----------
    def _collect_service_items(self) -> List[Dict[str, str]]:
        """
        Returns [{'code': <service_code>, 'name': <friendly_name>} ...]
        """
        try:
            params = self._ssm_list_parameters(
                "/aws/service/global-infrastructure/services",
                recursive=False,
                max_results=10
            )
        except (NoCredentialsError, BotoCoreError, ClientError) as e:
            error(f"SSM list services failed: {e}")
            params = []

        service_codes = [p.get("Value") for p in params if p.get("Value")]
        if not service_codes:
            try:
                deep = self._ssm_list_parameters(
                    "/aws/service/global-infrastructure/services",
                    recursive=True,
                    max_results=10
                )
                seen = set()
                for pp in deep:
                    name = pp.get("Name", "")
                    if "/services/" in name:
                        code = name.split("/services/")[-1].split("/")[0]
                        if code:
                            seen.add(code)
                service_codes = sorted(seen)
            except (NoCredentialsError, BotoCoreError, ClientError) as e:
                error(f"SSM deep list failed: {e}")
                service_codes = []

        pricing_fallback = None
        items: List[Dict[str, str]] = []
        for code in service_codes:
            long_name = self._ssm_get_parameter_value(f"/aws/service/global-infrastructure/services/{code}/longName")
            if not long_name:
                long_name = self._ssm_get_parameter_value(f"/aws/service/global-infrastructure/services/{code}/name")
            if not long_name:
                if pricing_fallback is None:
                    pricing_fallback = self._pricing_index_names()
                long_name = pricing_fallback.get(code) or code
            items.append({"code": code, "name": long_name.strip()})

        # dedupe by code; stable sort by name
        seen: set[str] = set()
        deduped: List[Dict[str, str]] = []
        for it in items:
            if it["code"] in seen:
                continue
            seen.add(it["code"])
            deduped.append(it)
        deduped.sort(key=lambda t: t["name"].lower())
        return deduped

    # ---------- Botocore model & endpoints enrichment ----------
    def _service_model_meta(self, service_code: str) -> Dict[str, Any]:
        """
        Pulls technical metadata from botocore service-2 models:
        endpoint_prefix, protocol, signature_version, api_version, operations_count.
        """
        try:
            loader = self._botocore.get_component("data_loader")
            model = loader.load_service_model(service_code, "service-2")
        except (DataNotFoundError, Exception):
            return {}

        md = model.get("metadata", {}) if isinstance(model, dict) else {}
        ops = model.get("operations", {}) if isinstance(model, dict) else {}

        return {
            "endpoint_prefix": md.get("endpointPrefix"),
            "protocol": md.get("protocol"),
            "signature_version": md.get("signatureVersion"),
            "api_version": md.get("apiVersion"),
            "operations_count": (len(ops) if isinstance(ops, dict) else None),
        }

    def _resolver_endpoints(self, service_code: str, *, max_regions: Optional[int] = None) -> List[Dict[str, str]]:
        """
        Uses botocore's endpoint resolver to list available regions and hostnames.
        Returns [{'region': 'us-east-1', 'hostname': '...'}, ...]
        """
        try:
            resolver = self._botocore.get_component("endpoint_resolver")
            # primary AWS partition
            regions = resolver.get_available_endpoints(service_code, partition_name="aws") or []
            out: List[Dict[str, str]] = []
            for r in regions:
                try:
                    ep = resolver.construct_endpoint(service_code, r) or {}
                    out.append({"region": r, "hostname": ep.get("hostname")})
                except Exception:
                    out.append({"region": r, "hostname": None})
            if max_regions and out:
                out = out[:max_regions]
            return out
        except Exception:
            return []

    # ---------- Public: unified Topics ----------
    def get_topics(self, *, page: int = 1, per_page: int = 60, **_: Any) -> Dict[str, Any]:
        assert page >= 1 and per_page >= 1
        services = self._collect_service_items()
        start = (page - 1) * per_page
        end = start + per_page
        slice_ = services[start:end]

        topics: List[Dict[str, Any]] = []
        for svc in slice_:
            code = svc["code"]
            name = svc["name"]
            ext_url = f"https://aws.amazon.com/{code}/"
            topics.append({
                "service": name,
                "identifications": IdentificationsModel(
                    input_identification=code,
                    title_identification=name,
                    link_identification=ext_url,
                    img_link_identification=None,
                ),
            })

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

        Returns on success:
          {
            "identifications": IdentificationsModel(...),
            "input_data": {
              "meta": {
                "service_code", "service_name",
                "external_url", "docs_url",
                "pricing_offer_code", "pricing_index_hint",
                "endpoint_prefix", "protocol", "signature_version", "api_version", "operations_count"
              },
              "regions": [ { "region": "...", "hostname": "..." }, ... ]   # optional
            },
            "updated_at": "<iso>"
          }
        On error: input_data == {}
        """
        now_iso = datetime.now(timezone.utc).isoformat()
        raw = (input_identification or "").strip()
        if not raw:
            return {
                "identifications": IdentificationsModel(
                    input_identification=None,
                    title_identification=None,
                    link_identification=None,
                    img_link_identification=None,
                ),
                "input_data": {},
                "updated_at": now_iso,
            }

        # Known codes (from topics helpers)
        items = self._collect_service_items()
        by_code = {it["code"]: it["name"] for it in items}
        by_name_lower = {it["name"].lower(): it["code"] for it in items}

        service_code: Optional[str] = None
        service_name: Optional[str] = None

        # Try direct code, then exact name, then loose contains on name
        if raw in by_code:
            service_code = raw
            service_name = by_code[raw]
        elif raw.lower() in by_name_lower:
            service_code = by_name_lower[raw.lower()]
            service_name = by_code.get(service_code)
        else:
            # substring match on name
            for nm, code in by_name_lower.items():
                if raw.lower() in nm:
                    service_code = code
                    service_name = by_code.get(code)
                    break

        if not service_code:
            # last resort: accept raw as code and try botocore/pricing heuristics
            service_code = raw

        # Pricing meta/name assistance
        pricing_names = self._pricing_index_names()
        if not service_name:
            service_name = pricing_names.get(service_code) or service_code

        # Botocore model metadata
        model_meta = self._service_model_meta(service_code)

        # Regions (with hostnames) via resolver
        regions_block: List[Dict[str, str]] = []
        if include_regions:
            regions_block = self._resolver_endpoints(service_code, max_regions=max_regions)

        # Pricing offer code & index hint
        pricing_offer_code = None
        for key, disp in pricing_names.items():
            if disp and disp.strip().lower() == (service_name or "").strip().lower() and key != service_code and key[0].isupper():
                pricing_offer_code = key
                break
        pricing_index_hint = (
            f"offers/v1.0/aws/{pricing_offer_code}/current/index.json" if pricing_offer_code else None
        )

        # URLs
        external_url = f"https://aws.amazon.com/{service_code}/"
        # docs are not perfectly uniform; provide a conservative best-effort root
        docs_url = f"https://docs.aws.amazon.com/{service_code}/"  # may not exist for all; still useful as a hint

        meta: Dict[str, Any] = {
            "service_code": service_code,
            "service_name": service_name or service_code,
            "external_url": external_url,
            "docs_url": docs_url,
            "pricing_offer_code": pricing_offer_code,
            "pricing_index_hint": pricing_index_hint,
            "endpoint_prefix": model_meta.get("endpoint_prefix"),
            "protocol": model_meta.get("protocol"),
            "signature_version": model_meta.get("signature_version"),
            "api_version": model_meta.get("api_version"),
            "operations_count": model_meta.get("operations_count"),
        }

        # If we failed to resolve anything meaningful beyond the raw code, still return a valid shape
        input_data: Dict[str, Any] = {"meta": meta}
        if regions_block:
            input_data["regions"] = regions_block

        return {
            "identifications": IdentificationsModel(
                input_identification=service_code,
                title_identification=service_name or service_code,
                link_identification=external_url,
                img_link_identification=None,
            ),
            "input_data": input_data,
            "updated_at": now_iso,
        }

    # ---------- Instructions ----------
    def instructions(self) -> str:
        return (
            "You will receive concise context about an AWS service: code, friendly name, external/docs URLs, "
            "optional pricing identifiers, technical metadata (endpoint prefix, protocol, signature version, API version, "
            "operation count), and region hostnames. Write clear, factual questions (SERIOUS mode). "
            "Prefer questions that test recognition of service purpose/name/code, endpoint/region coverage, "
            "and metadata comprehension. Avoid speculation."
        )
    
        # ---------- Search (local over service codes & friendly names) ----------
    def search(
        self,
        q: str,
        *,
        page: int = 1,
        per_page: int = 60,
        mode: str = "fulltext",                # "fulltext" | "substring" | "fuzzy"
        include_endpoint_prefix: bool = True,  # enriquece o haystack com endpoint_prefix
        **kwargs: Any,                         # <- exigido: absorve extras sem quebrar
    ) -> Dict[str, Any]:
        """
        Busca por serviços AWS por código/nome (e, opcionalmente, endpoint_prefix).
        - Carrega a lista via SSM (com fallback no índice de pricing).
        - Monta um haystack: code | name | pricing_display | endpoint_prefix(opcional).
        - Filtra por substring (ou fuzzy) e pagina o resultado filtrado.
        - Retorna o mesmo envelope de /topics com 'identifications' (IdentificationsModel).
        """
        assert isinstance(q, str) and q.strip(), "q deve ser não-vazio"
        assert page >= 1 and per_page >= 1
        qn = q.casefold()
        mode = mode if mode in ("fulltext", "substring", "fuzzy") else "fulltext"

        # 1) Base de serviços (code + friendly name)
        services = self._collect_service_items()  # [{'code': 'ec2', 'name': 'Amazon EC2'}, ...]

        # 2) Mapa de nomes do índice de pricing (ajuda a achar sinônimos)
        pricing_names = self._pricing_index_names()  # {'ec2': 'Amazon EC2', 'AmazonEC2': 'Amazon EC2', ...}

        # 3) (opcional) endpoint_prefix p/ melhorar o recall (ex: 'monitoring' p/ CloudWatch)
        ep_cache: Dict[str, Optional[str]] = {}
        def _endpoint_prefix(code: str) -> Optional[str]:
            if not include_endpoint_prefix:
                return None
            if code in ep_cache:
                return ep_cache[code]
            md = self._service_model_meta(code)
            ep_cache[code] = md.get("endpoint_prefix")
            return ep_cache[code]

        # 4) matching
        def _match(code: str, name: str) -> bool:
            fields = [code or "", name or ""]
            disp = pricing_names.get(code) or ""
            if disp:
                fields.append(disp)
            ep = _endpoint_prefix(code)
            if ep:
                fields.append(ep)

            hay = " ".join(x for x in fields if x).casefold()
            if not hay:
                return False
            if mode in ("fulltext", "substring"):
                return qn in hay
            # fuzzy
            return self._simple_fuzzy_score(hay, qn) >= 0.78

        matched: List[Dict[str, Any]] = []
        for it in services:
            code = it.get("code") or ""
            name = it.get("name") or code
            if not code:
                continue
            if not _match(code, name):
                continue

            ext_url = f"https://aws.amazon.com/{code}/"
            topic = {
                "service": name,
                "identifications": IdentificationsModel(
                    input_identification=code,
                    title_identification=name,
                    link_identification=ext_url,
                    img_link_identification=None,
                ),
            }
            matched.append(topic)

        # 5) ordenação estável (alfabética pelo title_identification)
        matched.sort(key=lambda t: (t["identifications"].title_identification or "").casefold())

        # 6) paginação sobre o resultado filtrado
        start = (page - 1) * per_page
        end = start + per_page
        topics_page = matched[start:end]
        has_more = end < len(matched)

        return {
            "topics": topics_page,
            "page": page,
            "per_page": per_page,
            "has_more": has_more,
            "fetched_at": datetime.now(timezone.utc).isoformat(),
            "item_name": self.item_name,
            "source_name": self.source_name,
        }


     # ---------------- context ----------------
    def generate_context(self, input_data: Dict[str, Any], amount_question: int = 10) -> str:
        """
        Builds a plain-text context string combining all key/value pairs in input_data
        and the model output structure, separated by newlines.
        """

        context_lines: list[str] = []

        # Safely iterate key/value pairs — stringify everything
        for key, value in (input_data or {}).items():
            # Represent complex values like dicts/lists in a readable way
            if isinstance(value, (dict, list, tuple, set)):
                context_lines.append(f"{key}: {json.dumps(value, ensure_ascii=False)}")
            else:
                context_lines.append(f"{key}: {value}")

        # Add your output structure
        output_structure = self.context_output_structure(amount_question=amount_question)
        context_lines.append(str(output_structure))

        # Join them all with newline separators
        return "\n".join(context_lines)
