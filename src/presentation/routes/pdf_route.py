from fastapi import APIRouter, Query, Response, status, APIRouter, UploadFile, File, HTTPException
from fastapi.responses import JSONResponse

from src.dal.local.pdf_adapter import PdfAdapter, PdfParser
from ..handler.responses import MyResponse

pdf_router = APIRouter()

@pdf_router.post("/pdf/topic", response_model=MyResponse)
async def ingest_pdf(response: Response,
    file: UploadFile = File(..., description="PDF file"),
    ocr_force: bool = False,
    ocr_lang: str = "eng",
    ocr_dpi: int = 300,
    max_chars: int = 8000,
    overlap_chars: int = 400,
):
    # quick content-type hint (adapter also verifies PDF header)
    if file.content_type and file.content_type not in ("application/pdf", "application/octet-stream"):
        raise HTTPException(status_code=415, detail="Only PDF uploads are supported.")

    try:
        raw = await file.read()

        # 1) Adapt: validate + metadata + scan detection
        adapter = PdfAdapter(raw, file.filename)
        adapter.validate()

        av_result = adapter.av_scan_clamav()                     # {"status": CLEAN|FOUND|UNKNOWN, ...}
        yara_result = adapter.yara_scan(rules_path=None)         # set a rules file if you have one

        if av_result.get("status") == "FOUND":
            raise HTTPException(
                status_code=400,
                detail=f"Malware detected by {av_result['engine']}: {av_result.get('signature')}"
            )
        
        if yara_result.get("matches"):
            raise HTTPException(
                status_code=400,
                detail=f"Malware detected by YARA: {', '.join(yara_result.get('matches', []))}"
            )

        meta = adapter.get_metadata()
        scan_score = adapter.scan_ratio()

        # 2) Parse: structure + OCR (auto-enable if likely scanned or user forces)
        parser = PdfParser(adapter.as_bytes_io(), adapter.filename)
        parsed = parser.parse(
            ocr_force=ocr_force or (scan_score >= 0.6),
            ocr_lang=ocr_lang,
            ocr_dpi=ocr_dpi,
            max_chars=max_chars,
            overlap_chars=overlap_chars,
        )

        payload = {
            "metadata": {**meta, "scan_ratio": round(scan_score, 3)},
            "parsed": parsed,  # contains: document_id, source, elements, chunks
        }
        response.status_code = status.HTTP_200_OK
        return MyResponse(data=payload, message="PDF ingested successfully.")

    except ValueError as e:
        # from adapter.validate() or your own checks
        response.status_code = status.HTTP_400_BAD_REQUEST
        return MyResponse(data=None, message=str(e))
    except Exception as e:
        # unexpected errors
        response.status_code = status.HTTP_500_INTERNAL_SERVER_ERROR
        return MyResponse(data=None, message="Internal server error: " + str(e))