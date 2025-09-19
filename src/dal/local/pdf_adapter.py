# pip install pymupdf
import io
import subprocess
import tempfile
import fitz  
from uuid import uuid4
import uuid
from typing import List, Dict, Any, Optional, Tuple
from PIL import Image
import pytesseract
from unstructured.partition.pdf import partition_pdf
from src.domain.models.pdf_model import  AntivirusScan, ElementAttrs, ParsedDocument, PdfChunk, PdfElement, PdfMetadata, YaraScan
from src.core.logs import error
try:
    import clamd
except Exception:
    clamd = None

class PdfAdapter:
    """
    Low-level adapter for PDF ingestion.
    - Validates input
    - Provides basic metadata
    - Ensures safe bytes stream
    - Detects 'scanned-like' PDFs (low embedded text)
    """

    def __init__(self, raw_bytes: bytes, filename: str = None):
        self.raw_bytes = raw_bytes
        self.filename = filename or f"{uuid4()}.pdf"
        self._doc = None

    def validate(self, max_size_mb: int = 20) -> None:
        if len(self.raw_bytes) > max_size_mb * 1024 * 1024:
            raise ValueError(f"PDF too large (> {max_size_mb} MB).")
        if not self.raw_bytes.startswith(b"%PDF"):
            raise ValueError("File does not look like a valid PDF.")
        # rudimentary embedded-action/JS checks
        _not_allowed = []
        _not_allowed.append(b"/JavaScript")
        # _not_allowed.append(b"/JS")
        #_not_allowed.append(b"/AA")
        #_not_allowed.append(b"/OpenAction")
        _not_allowed.append(b"/Launch")


        for sig in _not_allowed:
            if sig in self.raw_bytes:
                error(f"Suspicious embedded code detected: {sig.decode('latin1')}")
                raise ValueError("Suspicious embedded code detected in PDF.")

    def open(self):
        self._doc = fitz.open(stream=self.raw_bytes, filetype="pdf")
        return self._doc

    def get_metadata(self) -> Dict[str, Any]:
        if self._doc is None:
            self.open()
        meta = self._doc.metadata or {}
        m = PdfMetadata(
        filename=self.filename,
        pages=self._doc.page_count,
        title=meta.get("title"),
        author=meta.get("author"),
        subject=meta.get("subject"),
        creation_date=meta.get("creationDate"),
        mod_date=meta.get("modDate"),
        producer=meta.get("producer"),
        encrypted=self._doc.is_encrypted,
        )
        return m.to_dict()

    def scan_ratio(self, sample_pages: int = 5) -> float:
        """
        Heuristic: 0.0 (pure text) → 1.0 (likely scanned).
        Looks at a few pages; if little to no extractable text, treats as scanned.
        """
        if self._doc is None:
            self.open()

        n = self._doc.page_count
        if n == 0:
            return 1.0

        # pick evenly spaced sample pages
        idxs = sorted({int(i) for i in [0, n-1] + [round((n-1)*k/(sample_pages-1)) for k in range(sample_pages)] if 0 <= i < n})
        empties = 0
        for i in idxs:
            try:
                text = self._doc.load_page(i).get_text("text") or ""
                if len(text.strip()) < 20:  # tiny text → likely image page
                    empties += 1
            except Exception:
                empties += 1

        return empties / max(1, len(idxs))

    def as_bytes_io(self) -> io.BytesIO:
        return io.BytesIO(self.raw_bytes)

    def close(self):
        if self._doc:
            self._doc.close()

    def av_scan_clamav(self, host: str = "127.0.0.1", port: int = 3310) -> Dict[str, Any]:
        if clamd:
            try:
                cd = clamd.ClamdNetworkSocket(host=host, port=port)
                res = cd.instream(io.BytesIO(self.raw_bytes))
                status, sig = res.get('stream', ('UNKNOWN', None))
                status = "CLEAN" if status == "OK" else status
                return AntivirusScan(engine="clamav", status=status, signature=sig).to_dict()
            except Exception:
                pass
        try:
            with tempfile.NamedTemporaryFile(suffix=".pdf", delete=True) as tmp:
                tmp.write(self.raw_bytes); tmp.flush()
                proc = subprocess.run(
                    ["clamscan", "--stdout", "--no-summary", tmp.name],
                    capture_output=True, text=True, check=False,
                )
                out = proc.stdout.strip()
                if out.endswith(": OK"):
                    return AntivirusScan(engine="clamscan", status="CLEAN", signature=None).to_dict()
                if "FOUND" in out:
                    sig = out.split(":")[-1].replace("FOUND", "").strip() or "FOUND"
                    return AntivirusScan(engine="clamscan", status="FOUND", signature=sig).to_dict()
                return AntivirusScan(engine="clamscan", status="UNKNOWN", signature=None).to_dict()
        except FileNotFoundError:
            return AntivirusScan(engine="clamav", status="UNAVAILABLE", signature=None).to_dict()

    def yara_scan(self, rules_path: Optional[str] = None) -> Dict[str, Any]:
        try:
            import yara
            if not rules_path:
                return YaraScan(status="SKIPPED", matches=[]).to_dict()
            rules = yara.compile(filepath=rules_path)
            matches = rules.match(data=self.raw_bytes)
            status = "MATCH" if matches else "CLEAN"
            return YaraScan(status=status, matches=[m.rule for m in matches]).to_dict()
        except Exception:
            return YaraScan(status="ERROR", matches=[]).to_dict()





class PdfParser:
    """
    Hybrid PDF parser:
    - Uses 'unstructured' for layout-aware extraction (headings, tables, etc.)
    - Falls back to page-level OCR when a page has little/no embedded text
    - Produces AI-ready JSON with elements + chunks
    """

    def __init__(self, pdf_stream: io.BytesIO, filename: str):
        self.pdf_stream = pdf_stream
        self.filename = filename

    # ------------- Public API -------------

    def parse(
    self,
    ocr_force: bool = False,
    ocr_lang: str = "eng",
    ocr_dpi: int = 300,
    max_chars: int = 8000,
    overlap_chars: int = 400,
    ) -> Dict[str, Any]:
        """
        ocr_force=True → OCR every page (useful for heavily scanned docs).
        ocr_lang: e.g., "eng+por" for multilingual.
        """
        # 1) Try unstructured first (best for structure + tables)
        elements = partition_pdf(
            file=self.pdf_stream,
            # "hi_res" triggers OCR internally in some cases, but we’ll control OCR ourselves
            strategy="fast",
            include_page_breaks=True,
            infer_table_structure=True,
        )

        # Element dicts (public shape preserved)
        elements_json = [self._element_to_dict(e) for e in elements]
        pages_with_text = self._pages_with_text(elements_json)

        # 2) OCR pass (forced or sparse pages)
        ocr_extra_elements: List[Dict[str, Any]] = []
        with fitz.open(stream=self.pdf_stream.getvalue(), filetype="pdf") as doc:
            for page_no in range(doc.page_count):
                need_ocr = ocr_force or (page_no + 1) not in pages_with_text
                if not need_ocr:
                    continue
                ocr_text, conf = self._ocr_page(doc, page_no, dpi=ocr_dpi, lang=ocr_lang)
                if ocr_text.strip():
                    ocr_extra_elements.append({
                        "type": "OCRParagraph",
                        "text": ocr_text.strip(),
                        "page": page_no + 1,
                        "attrs": {"ocr": True, "ocr_confidence": conf},
                    })

        # 3) Merge OCR elements (append; caller may de-duplicate if desired)
        all_elements = self._merge_elements(elements_json, ocr_extra_elements)

        # 4) Build chunks (dicts first to preserve current chunker)
        chunks_dicts = self._chunk_elements(all_elements, max_chars=max_chars, overlap=overlap_chars)

        # 5) Convert dicts → dataclasses → final dict (typed schema, same outward shape)
        #    NOTE: assumes these are imported:
        #    from src.dal.local.pdf_models import ElementAttrs, PdfElement, PdfChunk, ParsedDocument
        typed_elements: List[PdfElement] = []
        for el in all_elements:
            attrs = el.get("attrs", {}) or {}
            bbox = attrs.get("bbox")
            bbox_tuple = tuple(bbox) if bbox else None
            typed_elements.append(
                PdfElement(
                    type=el.get("type", ""),
                    text=el.get("text", "") or "",
                    page=el.get("page"),
                    attrs=ElementAttrs(
                        bbox=bbox_tuple,
                        level=attrs.get("level"),
                        table_markdown=attrs.get("table_markdown"),
                        ocr=attrs.get("ocr"),
                        ocr_confidence=attrs.get("ocr_confidence"),
                    ),
                )
            )

        typed_chunks: List[PdfChunk] = []
        for ch in chunks_dicts:
            typed_chunks.append(
                PdfChunk(
                    chunk_id=ch.get("chunk_id", str(uuid.uuid4())),
                    text=ch.get("text", "") or "",
                    pages=[int(p) for p in (ch.get("pages") or [])],
                    section_path=list(ch.get("section_path") or []),
                    tokens_est=int(ch.get("tokens_est") or 0),
                )
            )

        doc = ParsedDocument(
            document_id=str(uuid.uuid4()),
            source=self.filename,
            elements=typed_elements,
            chunks=typed_chunks,
        )
        return doc.to_dict()

    # ------------- Internals -------------

    def _element_to_dict(self, el) -> Dict[str, Any]:
        # -> now returns a dict built via PdfElement (keeps the public shape identical)
        typ = getattr(el, "category", None) or el.__class__.__name__
        page = getattr(el.metadata, "page_number", None)

        # bbox
        bbox = None
        coords = getattr(el, "coordinates", None)
        if coords and getattr(coords, "points", None):
            xs = [p[0] for p in coords.points]
            ys = [p[1] for p in coords.points]
            bbox = (min(xs), min(ys), max(xs), max(ys))

        attrs = ElementAttrs(
            bbox=bbox,
            table_markdown=el.text if "Table" in typ else None,
        )
        model = PdfElement(
            type=typ,
            text=(el.text or "").strip(),
            page=page,
            attrs=attrs,
        )
        return model.to_dict()

    def _pages_with_text(self, elements: List[Dict[str, Any]]) -> set:
        pages = set()
        for el in elements:
            if (el.get("text") or "").strip() and el.get("page"):
                if len(el["text"]) > 20:  # ignore tiny tokens
                    pages.add(el["page"])
        return pages

    def _ocr_page(self, doc: fitz.Document, page_index: int, dpi: int, lang: str) -> Tuple[str, Optional[float]]:
        """
        Render page → image → Tesseract OCR.
        Returns (text, average_confidence). Confidence is a coarse mean; Tesseract
        Python wrapper doesn’t expose per-word conf by default via image_to_string.
        """
        page = doc.load_page(page_index)
        # scale to dpi
        zoom = dpi / 72.0
        mat = fitz.Matrix(zoom, zoom)
        pix = page.get_pixmap(matrix=mat, alpha=False)
        img = Image.frombytes("RGB", [pix.width, pix.height], pix.samples)

        # OCR
        config = "--oem 3 --psm 1"  # LSTM, assume block of text
        text = pytesseract.image_to_string(img, lang=lang, config=config)

        # optional confidence (rough): run data call; compute mean conf ignoring -1
        try:
            data = pytesseract.image_to_data(img, lang=lang, config=config, output_type=pytesseract.Output.DICT)
            confs = [int(c) for c in data.get("conf", []) if c not in ("-1", -1)]
            avg_conf = (sum(confs) / len(confs)) if confs else None
        except Exception:
            avg_conf = None

        return text, avg_conf

    def _merge_elements(
        self,
        elements_json: List[Dict[str, Any]],
        ocr_elements: List[Dict[str, Any]],
    ) -> List[Dict[str, Any]]:
        """
        Simple append. If you want to avoid duplicates on mixed pages:
        - If page already had non-trivial text, *but* OCR still ran (ocr_force),
          keep both but mark OCR elements; callers can filter by attrs.ocr.
        """
        if not ocr_elements:
            return elements_json
        merged = list(elements_json)
        merged.extend(ocr_elements)
        return merged

    def _chunk_elements(
    self, elements: List[Dict[str, Any]], max_chars: int = 8000, overlap: int = 400
    ) -> List[Dict[str, Any]]:
        buffer = ""
        blocks: List[Dict[str, Any]] = []
        pages = set()
        section_path: List[str] = []

        def flush():
            nonlocal buffer, pages
            if not buffer.strip():
                return
            ch = PdfChunk(
                chunk_id=str(uuid.uuid4()),
                text=buffer.strip(),
                pages=sorted(list(pages)),
                section_path=section_path[-3:],
                tokens_est=len(buffer) // 4,
            )
            blocks.append(ch.to_dict())
            buffer = ""
            pages = set()

        for el in elements:
            pg = el.get("page")
            if pg:
                pages.add(pg)

            t = (el.get("type") or "").lower()
            txt = (el.get("text") or "").strip()
            if not txt:
                continue

            if "title" in t or "header" in t:
                flush()
                section_path.append(txt)
                buffer += f"# {txt}\n\n"
            elif "table" in t:
                md = el.get("attrs", {}).get("table_markdown") or txt
                buffer += "\n" + md.strip() + "\n\n"
            else:
                buffer += txt + "\n\n"

            if len(buffer) > max_chars:
                cut = buffer[:max_chars]
                overlap_txt = buffer[max_chars - overlap:]
                ch = PdfChunk(
                    chunk_id=str(uuid.uuid4()),
                    text=cut.strip(),
                    pages=sorted(list(pages)),
                    section_path=section_path[-3:],
                    tokens_est=len(cut) // 4,
                )
                blocks.append(ch.to_dict())
                buffer = overlap_txt
                pages = set()

        flush()
        return blocks
