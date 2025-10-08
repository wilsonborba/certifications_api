


import datetime
from typing import Any, Dict
from fastapi import UploadFile
from src.core.settings import app_settings
from src.domain.models.quiz_result_model import QuizResultModel
from src.core.logs import error

from src.presentation.handler.responses import AIGenerationError, MalwareDetectedError
from src.dal.local.pdf_adapter import PdfAdapter
from src.domain.services.quiz_base import BaseQuizManager


settings = app_settings()


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
    
    def save_questions(
        self,
        response: dict[str, any],
        document_id: str,
        amount_question: int,
        attempt_index: int,
        user_uuid_id: str,
        *args,
        **kwargs
        ) -> QuizResultModel:

        """Will save question into Redis, (bcuz if the user didnt start the quiz, no need to save into main DB )"""

        items = response.get("questions", []) or []

        saved_questions = []

        questions_pdf_id = self.redis_adapter.k(
        settings.QUESTIONS_PREFIX,
        document_id,
        attempt_index
        )

        for idx, item in enumerate(items):
            qtext = (item.get("question") or "").strip()
            correct = item.get("correct_answer")
            options = item.get("options", []) or []
            difficulty = item.get("difficulty")
            justification = item.get("justification")


            question_data = {
                "question_id": idx + 1,
                "user_uuid_id": user_uuid_id,
                "question": qtext,
                "correct_answer": correct,
                "options": options,
                "difficulty": difficulty,
                "justification": justification
            }

        



        saved_questions.append(question_data)

        self.redis_adapter.set(questions_pdf_id, saved_questions, ex=1800 + (amount_question * 60))  # 30 min + amount of question == minutes expiration

        # remove correct answer from options for quiz taking, and justification and remove user_uuid_id
        for q in saved_questions:
            q.pop("correct_answer", None)
            q.pop("justification", None)
            q.pop("user_uuid_id", None)


        return QuizResultModel(
            saved_questions=saved_questions,
            identification=document_id,
            created_at=datetime.now().isoformat()
        )

    def get_questions(self):
        pass

    async def generate_context(self, input_data, amount_question, selected_language: str = "English") -> str:

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


        prompt = self.pdf_adapter.generate_context(
            input_data=input_data,
            amount_question=amount_question,
            )

        try:
            response = await self.pdf_adapter.gemini.generate_text(
                prompt=prompt,
                system_instruction=self.pdf_adapter.instructions(selected_language=selected_language),
                response_mime_type="application/json",
                temperature=0.7,
            )
            return response
        except Exception as e:
            error(f"AI generation error: {e}")
            raise AIGenerationError("Failed to generate context from AI service.")






    