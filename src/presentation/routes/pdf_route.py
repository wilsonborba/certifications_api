from fastapi import APIRouter, Query, Request, Response, status, APIRouter, UploadFile, File, HTTPException
from fastapi.responses import JSONResponse

from src.presentation.handler.pdf_handler import cache_get, cache_set, filter_payload_by_pages, get_topic_from_pdf, parse_page_selector
from ..handler.responses import MalwareDetectedError, MyResponse, UnsupportedFileTypeError
from src.core.logs import error

pdf_router = APIRouter()

@pdf_router.post("/pdf/topic", response_model=MyResponse)
async def get_topic_pdf(
    response: Response,
    request: Request,
    file: UploadFile = File(..., description="PDF file"),
    ocr_force: bool = False,
    ocr_lang: str = "eng",
    ocr_dpi: int = 300,
    max_chars: int = 8000,
    overlap_chars: int = 400,
):
    
    try:
        cache = request.app.state.redis 

        payload = await get_topic_from_pdf(
            file=file,
            ocr_force=ocr_force,
            ocr_lang=ocr_lang,
            ocr_dpi=ocr_dpi,
            max_chars=max_chars,
            overlap_chars=overlap_chars,
        )
        doc_id = payload["parsed"]["document_id"]
        await cache_set(cache, doc_id, payload)

        

        response.status_code = status.HTTP_200_OK
        return MyResponse(
            data=payload,
            message="PDF ingested and cached for 30 minutes.",
        )

    except ValueError as e:
        # from adapter.validate() or your own checks
        response.status_code = status.HTTP_400_BAD_REQUEST
        error(f"PDF ingestion error: {e}")
        return MyResponse(data=None, message=f"A bad request was made, please check your input...")
    except UnsupportedFileTypeError as e:
        response.status_code = status.HTTP_415_UNSUPPORTED_MEDIA_TYPE
        error(f"Unsupported file type: {e}")
        return MyResponse(data=None, message=str(e))
    except MalwareDetectedError as e:
        response.status_code = status.HTTP_409_CONFLICT
        return MyResponse(data=None, message=str(e))
    except Exception as e:
        # unexpected errors
        response.status_code = status.HTTP_500_INTERNAL_SERVER_ERROR
        error(f"Internal server error: {e}")
        return MyResponse(data=None, message="Internal server error...")

 
@pdf_router.get("/pdf/input/{document_id}", response_model=MyResponse)
async def get_pdf_input(
    response: Response,
    request: Request,
    document_id: str,
    # selected
    selected_pages: str = Query('all', description="Selected pages in format '1,2,5-10' or 'all'/'-4'/'2-'"),
):
    try:
        cache = request.app.state.redis
        found, cached, ttl = await cache_get(cache, document_id)
        if not found or ttl == -2:
            response.status_code = status.HTTP_404_NOT_FOUND
            return MyResponse(data=None, message="Document not found or cache expired.")

        total_pages = int(cached.get("metadata", {}).get("pages") or 0)
        if total_pages <= 0:
            response.status_code = status.HTTP_422_UNPROCESSABLE_ENTITY
            return MyResponse(data=None, message="Cached document has no page metadata.")

        # Validate + compute pages
        if selected_pages:
            try:
                pages = parse_page_selector(selected_pages, total_pages)
            except ValueError as e:
                response.status_code = status.HTTP_400_BAD_REQUEST
                return MyResponse(data=None, message=str(e))

        if not pages:
            response.status_code = status.HTTP_400_BAD_REQUEST
            return MyResponse(data=None, message="No valid pages selected.")

        filtered = filter_payload_by_pages(cached, pages)



        response.status_code = status.HTTP_200_OK
        return MyResponse(
            data=filtered,
            message=f"Returning {len(pages)} selected page(s)."
        )

    except Exception:
        response.status_code = status.HTTP_500_INTERNAL_SERVER_ERROR
        return MyResponse(data=None, message="Internal server error...")