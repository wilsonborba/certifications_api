from fastapi import APIRouter, Query, Response, status, APIRouter, UploadFile, File, HTTPException
from fastapi.responses import JSONResponse

from src.dal.local.pdf_adapter import PdfAdapter, PdfParser
from ..handler.responses import MyResponse
from src.core.logs import error

pdf_router = APIRouter()

@pdf_router.post("/pdf/topic", response_model=MyResponse)
async def get_topic_pdf(response: Response,
    file: UploadFile = File(..., description="PDF file"),
    ocr_force: bool = False,
    ocr_lang: str = "eng",
    ocr_dpi: int = 300,
    max_chars: int = 8000,
    overlap_chars: int = 400,
):
    # quick content-type hint (adapter also verifies PDF header)
    if file.content_type and file.content_type not in ("application/pdf", "application/octet-stream"):
        response.status_code = status.HTTP_415_UNSUPPORTED_MEDIA_TYPE
        return MyResponse(data=None, message="Only PDF uploads are supported.")

    try:
        raw = await file.read()

        # 1) Adapt: validate + metadata + scan detection
        adapter = PdfAdapter(raw, file.filename)
        adapter.validate()

        av_result = adapter.av_scan_clamav()                     # {"status": CLEAN|FOUND|UNKNOWN, ...}
        yara_result = adapter.yara_scan(rules_path=None)         # set a rules file if you have one

        if av_result.get("status") == "FOUND":
            error(f"Malware detected by {av_result['engine']}: {av_result.get('signature')}")
            response.status_code = status.HTTP_409_CONFLICT
            return MyResponse(data=None, message=f"Malware detected...")

        if yara_result.get("matches"):
            error(f"Malware detected by YARA: {', '.join(yara_result.get('matches', []))}")
            response.status_code = status.HTTP_409_CONFLICT
            return MyResponse(data=None, message=f"Malware detected...")
            

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
        error(f"PDF ingestion error: {e}")
        return MyResponse(data=None, message=f"A bad request was made, please check your input...")
    except Exception as e:
        # unexpected errors
        response.status_code = status.HTTP_500_INTERNAL_SERVER_ERROR
        error(f"Internal server error: {e}")
        return MyResponse(data=None, message="Internal server error...")
    
@pdf_router.get("/pdf/input/{document_id}", response_model=MyResponse)
async def get_pdf_input(response: Response,
                        selected_pages: str = Query(..., description="Selected pages in format '1,2,5-10'"),
                        ):
    # This is a placeholder for the actual implementation
    response.status_code = status.HTTP_200_OK
    return MyResponse(data={"message": "PDF input retrieved successfully."})