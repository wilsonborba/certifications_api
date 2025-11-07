


from datetime import datetime, timezone
from typing import Any, Dict, Optional
from fastapi import UploadFile
from src.dal.local.redis_adapter import RedisAdapter
from src.core.settings import app_settings
from src.domain.models.quiz_result_model import QuizResultModel
from src.core.logs import error, debug

from src.presentation.handler.responses import AIGenerationError, MalwareDetectedError
from src.dal.local.pdf_adapter import PdfAdapter
from src.domain.services.quiz_base import BaseQuizManager
import uuid

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
    
    async def save_questions(
        self,
        response: dict[str, any],
        redis_adapter: RedisAdapter,
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

        questions_pdf_id = redis_adapter.k(
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
                "pdf_question_id": uuid.uuid4().hex,
                "user_uuid_id": user_uuid_id,
                "question": qtext,
                "correct_answer": correct,
                "options": options,
                "difficulty": difficulty,
                "justification": justification,
                "complaint_text": None,
            }

            saved_questions.append(question_data)

        await redis_adapter.set(questions_pdf_id, saved_questions, ex=1800 + (amount_question * 60))  # 30 min + amount of question == minutes expiration

        # remove correct answer from options for quiz taking, and justification and remove user_uuid_id
        for q in saved_questions:
            q.pop("correct_answer", None)
            q.pop("justification", None)
            q.pop("user_uuid_id", None)


        return QuizResultModel(
            saved_questions=saved_questions,
            identification=document_id,
            created_at=datetime.now(timezone.utc).isoformat()
        )
    
    async def save_complaint(
        self,
        redis_adapter: RedisAdapter,
        user_uuid_id: str,
        complaint_text: str,
        document_id: str,
        pdf_question_id: str,
    ) -> dict:
        ok = await self.update_question(   # <-- await
            redis_adapter=redis_adapter,
            document_id=document_id,
            key="complaint_text",
            value=complaint_text,
            where_clause={"pdf_question_id": pdf_question_id},
        )
        # (optional) check ok and log/raise if needed
        return {
            # "user_uuid_id": user_uuid_id,
            "complaint_text": complaint_text,
            "document_id": document_id,
            "pdf_question_id": pdf_question_id,
        }

    async def get_questions(self, redis_adapter: RedisAdapter, document_id: str) -> list[dict]:

        questions = []
        for idx in (1, 2):
            k = redis_adapter.k(settings.QUESTIONS_PREFIX, document_id, idx)
            data = await redis_adapter.get(k)
            if isinstance(data, list):
                questions.extend(data)
        return questions

    async def update_question(
        self,
        redis_adapter: RedisAdapter,
        document_id: str,
        key: str,
        value: Any,
        where_clause: Optional[Dict[str, Any]] = None,
        *,
        update_many: bool = False,
    ) -> bool:
        where = where_clause or {}
        
        updated_any = False

        for idx in (1, 2):
            k = redis_adapter.k(settings.QUESTIONS_PREFIX, document_id, idx)
            data = await redis_adapter.get(k)
            if not isinstance(data, list):
                continue

            updated = 0
            for q in data:
                if isinstance(q, dict) and all(q.get(wk) == wv for wk, wv in where.items()):
                    q[key] = value
                    debug(f"Updated question in Redis {k}: set {key}={value} where {where}")
                    updated += 1
                    if not update_many:
                        break

            if updated:
                ttl = await redis_adapter.ttl(k)
                ex = ttl if isinstance(ttl, int) and ttl > 0 else None
                await redis_adapter.set(k, data, ex=ex)
                updated_any = True

                if not update_many:
                    break  # stop after first matching attempt if doing single update

        return updated_any




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






    