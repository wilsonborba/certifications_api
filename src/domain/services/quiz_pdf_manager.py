


from typing import Any, Dict
from fastapi import UploadFile
from src.core.logs import error

from src.presentation.handler.responses import AIGenerationError, MalwareDetectedError
from src.dal.local.pdf_adapter import PdfAdapter
from src.domain.services.quiz_base import BaseQuizManager


class QuizPDFManager(BaseQuizManager):

    def __init__(self, 
        raw: bytes | None = None,
        filename: str | None = None,
        *,
        upload: UploadFile | None = None,
        document_id: str | None = None):
        super().__init__()
        self.pdf_adapter = PdfAdapter(
            raw=raw,
            filename=filename,
            upload=upload,
            document_id=document_id
        )

    async def get_topics(self, file: UploadFile,
        ocr_force: bool = False,
        ocr_lang: str = "eng",
        ocr_dpi: int = 300,
        max_chars: int = 8000,
        overlap_chars: int = 400):
        
        pdf_adapter = await PdfAdapter.from_upload(file)
                   
        return pdf_adapter.get_topics(
        ocr_force=ocr_force,
        ocr_lang=ocr_lang,
        ocr_dpi=ocr_dpi,
        max_chars=max_chars,
        overlap_chars=overlap_chars
        )
    
    def get_input(self, selected_pages: str | None, total_pages: int, input_data) -> Dict[str, Any]:
        return self.pdf_adapter.get_input(selected_pages, total_pages, input_data)
    
    def save_questions(self):
        pass

    def get_questions(self):
        pass

    async def generate_context(self, input_data, amount_question):

        try:
            ai_injection_result = await self.pdf_adapter.check_ai_injection(input_data=input_data)

            ai_status = ai_injection_result.status
        
            if ai_status in ("SUSPECT", "MALICIOUS"):
                raise MalwareDetectedError(f"Malware AI Injection detected in document (status: {ai_status}).")
            
        except MalwareDetectedError:
            raise
        except RuntimeError:
            raise 
        except Exception as e:
            raise e


        prompt = self.pdf_adapter.generate_context(input_data=input_data, amount_question=amount_question)

        try:
            response = await self.pdf_adapter.gemini.generate_text(
                prompt=prompt,
                system_instruction=self.pdf_adapter.instructions(),
                response_mime_type="application/json",
                temperature=0.7,
            )
            return response
        except Exception as e:
            error(f"AI generation error: {e}")
            raise AIGenerationError("Failed to generate context from AI service.")






    