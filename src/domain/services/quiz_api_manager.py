from datetime import datetime, timedelta, timezone
from uuid import uuid4

from src.core.logs import debug, error, warning
from src.core.settings import app_settings
from src.dal.local.db_adapter import DBAdapter
from src.dal.remote.ai.ai_factory import AiFactory
from src.dal.remote.ai.gemini import GeminiClient
from src.domain.models.available_languages import is_valid_language
from src.domain.models.quiz_result_model import QuizResultModel
from src.domain.models.user_answer_model import UserAnswerModel
from src.domain.models.user_certification import UserCertificationModel
from src.domain.services.quiz_base import BaseQuizManager, _normalize_text, _sha256
from src.presentation.handler.responses import NoDefaultAIClientError


class QuizAPIManager(BaseQuizManager):
    def __init__(self):
        super().__init__()

        self.ai_factory = AiFactory()

    def ai_client_instance(self, client_name: str):
        ai_adapter = self.ai_factory.get_adapter(client_name)
        return ai_adapter

    def get_default_ai_client(self, user_uuid_id: str):
        db_user_token = self.db_adapter.read_where_one(
            "certifications_usertokens",
            {
                "is_default": True,
                "user_uuid_id": user_uuid_id,
            },
        )

        if db_user_token:
            client = self.ai_factory.get_adapter(db_user_token.get("provider_name"))
            if client:
                client.set_api_key(db_user_token.get("token_value"))
                return client

        return None

    def ai_client(self, user_uuid_id):
        default_client = self.get_default_ai_client(user_uuid_id)
        if default_client:
            return default_client
        else:
            raise NoDefaultAIClientError("No default AI client configured.")

    def process_quiz_revision(
        self,
        answers: list,
        time_spent_seconds: float,
        certification_title: str,
        full_name: str,
        language: str,
        user_uuid_id: str,
    ):
        ua_list = []
        for ans in answers:
            question_id = ans.get("questionId")
            selected_index = ans.get("selectedIndex")
            selected_text = ans.get("selectedText")

            answers_from_question = self.db_adapter.read_where_many(
                table_name="certifications_answer",
                where={
                    "question_id": question_id,
                    # "is_correct": True
                },
            )

            # debug(f"Answers from question {question_id}: {answers_from_question}")
            
            correct_answer_id = next(
                (
                    a.get("id")
                    for a in answers_from_question
                    if a.get("is_correct") in (True, 1)
                ),
                None,
            )

            if not answers_from_question:
                warning(
                    f"No answers found for question {question_id}; skipping user answer."
                )
                continue

            if correct_answer_id is None:
                warning(
                    f"No correct answer found for question {question_id}; skipping user answer."
                )
                continue
            # both text hashed for comparison
            if answers_from_question and selected_text:
                for ans in answers_from_question:
                    user_answer = UserAnswerModel()

                    if _normalize_text(ans.get("text", "")) == _normalize_text(
                        selected_text
                    ) and ans.get("is_correct") in (True, 1):
                        user_answer.user_certification_id = (
                            0  # Placeholder, should be set properly
                        )
                        user_answer.question_id = question_id
                        user_answer.correct_answer_id = correct_answer_id
                        user_answer.selected_answer_id = ans.get("id")
                        user_answer.is_correct = True
                        user_answer.user_uuid_id = user_uuid_id
                        user_answer.updated_at = datetime.now(timezone.utc).isoformat()
                        ua_list.append(user_answer.to_dict())

                    elif _normalize_text(ans.get("text", "")) == _normalize_text(
                        selected_text
                    ) and ans.get("is_correct") not in (True, 1):
                        user_answer.user_certification_id = (
                            0  # Placeholder, should be set properly
                        )
                        user_answer.question_id = question_id
                        user_answer.correct_answer_id = correct_answer_id
                        user_answer.selected_answer_id = ans.get("id")
                        user_answer.is_correct = False
                        user_answer.user_uuid_id = user_uuid_id
                        user_answer.updated_at = datetime.now(timezone.utc).isoformat()
                        ua_list.append(user_answer.to_dict())

            if not answers_from_question or not selected_text:
                # User did not select an answer or no answers available
                user_answer = UserAnswerModel()
                user_answer.user_certification_id = (
                    0  # Placeholder, should be set properly
                )
                user_answer.question_id = question_id
                user_answer.correct_answer_id = correct_answer_id
                user_answer.selected_answer_id = None
                user_answer.is_correct = False
                user_answer.user_uuid_id = user_uuid_id
                user_answer.updated_at = datetime.now(timezone.utc).isoformat()

                ua_list.append(user_answer.to_dict())

        correct_questions = sum(1 for ua in ua_list if ua["is_correct"])
        total_questions = len(ua_list)

        user_certification = UserCertificationModel()

        user_certification.certification_title = certification_title
        user_certification.uuid_certification = uuid4().hex

        user_certification.user_uuid_id = user_uuid_id
        user_certification.full_name = full_name
        user_certification.language = language
        # interval in seconds to isoformat
        user_certification.time_spent = str(timedelta(seconds=time_spent_seconds))
        user_certification.created_at = datetime.now(timezone.utc).isoformat()
        user_certification.is_pdf = True
        user_certification.correct_questions = correct_questions
        user_certification.wrong_questions = total_questions - correct_questions
        user_certification.total_questions = total_questions
        user_certification.score = (
            (correct_questions / total_questions) * 100 if total_questions > 0 else 0.0
        )

        certification_id = self.db_adapter.insert_row(
            "certifications_usercertification", user_certification.to_dict()
        )

        for ua in ua_list:
            ua["user_certification_id"] = certification_id
            _ = self.db_adapter.insert_row("certifications_useranswer", ua)

        return {
            # return uuid_certification_id instead of DB PK
            "certification_id": user_certification.uuid_certification,
            # "user_answers": ua_list
        }

    def save_questions(
        self,
        response: dict[str, any],
        *,
        item_name: str,
        input_identification: str,
        selected_language: str,
    ) -> QuizResultModel:
        """
        Persist Gemini questions for the given (item_name, input_identification).
        Skips exact duplicates by hash and near-duplicates by similarity rule (>=0.71).
        Returns summary: {"inserted": N, "skipped_exact": M, "skipped_similar": K}
        """

        items = response.get("questions", []) or []

        debug(f"Saving {len(items)} questions for {item_name} / {input_identification}")

        if not is_valid_language(selected_language):
            error(f"Invalid language selected: {selected_language}")
            raise ValueError("Invalid selected language.")

        source_item_db = self.db_adapter.read_where_one(
            "certifications_sourceitem", {"item_name": item_name}
        )
        if not source_item_db:
            error(f"Source item not found in DB for item_name: {item_name}")
            raise ValueError("Source item must be cached before saving questions.")

        input_db = self.db_adapter.read_where_one(
            "certifications_input",
            {
                "source_item_id": source_item_db["id"],
                "input_identification": input_identification,
            },
        )
        if not input_db:
            error(
                f"Input not found in DB for item_name: {item_name}, input_identification: {input_identification}"
            )
            raise ValueError("Input must be cached before saving questions.")

        inserted = 0
        skipped_exact = 0
        skipped_similar = 0

        saved_questions = []

        for q in items:
            qtext = (q.get("question") or "").strip()
            correct = q.get("correct_answer")
            options = q.get("options", []) or []
            difficulty = q.get("difficulty")
            justification = q.get("justification")
            pdf_question_id = q.get("pdf_question_id", {})

            if not qtext or correct is None or not options:
                error(f"Invalid question data (missing parts): {q}")
                continue

            correct_indices = set()
            correct_texts = set()
            if isinstance(correct, int):
                if 0 <= correct < len(options):
                    correct_indices.add(correct)
            elif isinstance(correct, str):
                stripped = correct.strip()
                if stripped.isdigit():
                    idx = int(stripped)
                    if 0 <= idx < len(options):
                        correct_indices.add(idx)
                elif len(stripped) == 1 and stripped.upper() in "ABCD":
                    idx = ord(stripped.upper()) - ord("A")
                    if 0 <= idx < len(options):
                        correct_indices.add(idx)
                else:
                    correct_texts.add(stripped)
            elif isinstance(correct, list):
                for item in correct:
                    if isinstance(item, str):
                        correct_texts.add(item.strip())

            if not correct_indices and not correct_texts:
                error(f"Invalid correct answer mapping for question: {q}")
                continue

            debug(f"Normalizing question text for {item_name} / {input_identification}")

            norm = _normalize_text(qtext)
            nhash = _sha256(norm)

            # Exact duplicate by normalized hash?

            pld = {
                "input_id": input_db["id"],
                "normalized_text_hash": nhash,
                "selected_language": selected_language,
            }

            existing = self.db_adapter.read_where_one("certifications_question", pld)

            if existing:
                skipped_exact += 1
                continue

            # 70% similarity rule (vector if possible, text otherwise)
            debug(f"Checking similarity for question: {qtext}")

            if self._is_too_similar(
                input_id=input_db["id"],
                candidate_text=qtext,
                candidate_norm=norm,
                cand_threshold=0.71,
            ):
                skipped_similar += 1
                continue

            # Optional: get an embedding now so we don’t have to backfill later
            cand_vec = self._embed_question_text(qtext)

            # Insert Question
            question_id = self.db_adapter.insert_row(
                "certifications_question",
                {
                    "input_id": input_db["id"],
                    "question_text": qtext,
                    "normalized_text": norm,
                    "normalized_text_hash": nhash,
                    "justification": justification,
                    "difficulty": difficulty,
                    "embedding": cand_vec,  # None is fine if you can't embed yet
                    "updated_at": datetime.now(timezone.utc).isoformat(),
                    "selected_language": q.get("selected_language"),
                },
            )

            saved_questions.append(
                {
                    "id": question_id,
                    "question_text": qtext,
                    "options": options,
                    "difficulty": difficulty,
                    "pdf_question_id": pdf_question_id,
                }
            )

            # Insert Answers
            for idx, opt in enumerate(options):
                opt_text = (opt or "").strip()
                if not opt_text:
                    continue
                opt_norm = _normalize_text(opt_text)
                opt_hash = _sha256(opt_norm)
                is_correct = idx in correct_indices or opt_text in correct_texts

                # NOTE: do NOT overwrite the 'inserted' counter with the DB return value.
                _ = self.db_adapter.insert_row(
                    "certifications_answer",
                    {
                        "question_id": question_id,
                        "text": opt_text,
                        "normalized_text": opt_norm,
                        "normalized_text_hash": opt_hash,
                        "is_correct": 1 if is_correct else 0,
                        "position": idx,
                        "updated_at": datetime.now(timezone.utc).isoformat(),
                    },
                )

            # Bump the inserted counter once per question persisted
            inserted += 1

        return QuizResultModel(
            saved_questions=saved_questions,
            identification=f"{item_name}:{input_identification}",
            created_at=datetime.now(timezone.utc).isoformat(),
        )
