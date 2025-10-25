from fastapi import APIRouter, Query, Request, Response, status, APIRouter, UploadFile, File, HTTPException
from fastapi.responses import JSONResponse

from src.presentation.handler.pdf_handler import  get_context_from_pdf, get_input_from_pdf, get_topic_from_pdf
from ..handler.responses import (
    AIGenerationError, DocumentNotFoundError, InvalidTotalPagesError,
      MalwareDetectedError, MyResponse, NotEnoughQuestionsGeneratedError, UnsupportedFileTypeError
      )
from src.core.logs import error
from src.dal.remote.gemini import GeminiError

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
            cache=cache,
        )

        

        response.status_code = status.HTTP_200_OK
        return MyResponse(
            data=payload,
            message="PDF ingested and cached for 30 minutes.",
        )

    except ValueError as e:
        # from adapter.validate() or your own checks
        error(f"PDF ingestion error: {e}")
        response.status_code = status.HTTP_400_BAD_REQUEST
        return MyResponse(data=None, message=f"A bad request was made, please check your input...")
    except UnsupportedFileTypeError as e:
        error(f"Unsupported file type: {e}")
        response.status_code = status.HTTP_415_UNSUPPORTED_MEDIA_TYPE
        return MyResponse(data=None, message=str(e))
    except MalwareDetectedError as e:
        error(f"Malware detected in uploaded file: {e}")
        response.status_code = status.HTTP_409_CONFLICT
        return MyResponse(data=None, message=str(e))
    except Exception as e:
        # unexpected errors
        error(f"Internal server error: {e}")
        response.status_code = status.HTTP_500_INTERNAL_SERVER_ERROR
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
        inputs = await get_input_from_pdf(
            request=request,
            document_id=document_id,
            selected_pages=selected_pages,
        )

        response.status_code = status.HTTP_200_OK
        return MyResponse(
            data=inputs,
            message="PDF input retrieved successfully.",
        )
    except ValueError as e:
        error(f"PDF input retrieval error: {e}")
        response.status_code = status.HTTP_400_BAD_REQUEST
        return MyResponse(data=None, message=f"A bad request was made, please check your input...")
    except DocumentNotFoundError as e:
        error(f"Document not found: {e}")
        response.status_code = status.HTTP_404_NOT_FOUND
        return MyResponse(data=None, message=str(e))
    
    except InvalidTotalPagesError as e:
        error(f"Invalid total pages: {e}")
        response.status_code = status.HTTP_412_PRECONDITION_FAILED
        return MyResponse(data=None, message=str(e))
    

    except Exception as e:
        error(f"Internal server error: {e}")
        response.status_code = status.HTTP_500_INTERNAL_SERVER_ERROR
        return MyResponse(data=None, message="Internal server error...")
    

@pdf_router.get("/pdf/context/{document_id}", response_class=JSONResponse)
async def get_pdf_context(
    response: Response,
    request: Request,
    document_id: str,
    # selected
    selected_pages: str = Query('all', description="Selected pages in format '1,2,5-10' or 'all'/'-4'/'2-'"),
    mode: str = Query('both', description="Mode: playful, serious, both"),
    selected_language: str = Query('English', description="Selected language for context generation"),
    amount_question: int = Query(10, description="Number of questions to generate")
):
    
    user_uuid_id = request.headers.get("x-uuid")
    
    try:
        ai_result = await get_context_from_pdf(
            request=request,
            document_id=document_id,
            selected_pages=selected_pages,
            amount_question=amount_question,
            selected_language=selected_language,
            user_uuid_id=user_uuid_id
        )
        response.status_code = status.HTTP_200_OK
        return MyResponse(
            data=ai_result,
            message="PDF context retrieved successfully.",
        )
    
    except NotEnoughQuestionsGeneratedError as e:
        error(f"Not enough questions generated: {e}")
        response.status_code = status.HTTP_422_UNPROCESSABLE_ENTITY
        return MyResponse(data=None, message=str(e))
    
    except GeminiError as e:
        # Map upstream → gateway-ish status
        upstream = int(getattr(e, "status_code", 502) or 502)
        mapped = 502 if upstream < 500 or upstream == 500 else (503 if upstream in (503,) else (504 if upstream == 504 else 502))
        error(f"Gemini error [{upstream}]: {e.payload}")
        response.status_code  = status.HTTP_503_SERVICE_UNAVAILABLE
        # keep message generic (don’t leak upstream details to clients)
        return MyResponse(
            data=None,
            message="Upstream AI service error. Please try again later."
        )
    
    except AIGenerationError as e:
        error(f"AI generation error: {e}")
        response.status_code  = status.HTTP_503_SERVICE_UNAVAILABLE
        return MyResponse(data=None, message=str(e))
    
    except MalwareDetectedError as e:
        error(f"Malware detected in document: {e}")
        response.status_code = status.HTTP_409_CONFLICT
        return MyResponse(data=None, message=str(e))


    except ValueError as e:
        error(f"PDF context retrieval error: {e}")
        response.status_code = status.HTTP_400_BAD_REQUEST
        return MyResponse(data=None, message=f"A bad request was made, please check your input...")
    
    except DocumentNotFoundError as e:
        error(f"Document not found: {e}")
        response.status_code = status.HTTP_404_NOT_FOUND
        return MyResponse(data=None, message=str(e))
    except InvalidTotalPagesError as e:
        error(f"Invalid total pages: {e}")
        response.status_code = status.HTTP_422_UNPROCESSABLE_ENTITY
        return MyResponse(data=None, message=str(e))
    
    
    except Exception as e:    
        error(f"Internal server error [{type(e).__name__}]: {e!r}")
        response.status_code = status.HTTP_500_INTERNAL_SERVER_ERROR
        return MyResponse(data=None, message="Internal server error...")
    


   