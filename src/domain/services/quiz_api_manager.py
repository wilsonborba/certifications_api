


from datetime import datetime, timezone
import json
from src.domain.models.quiz_result_model import QuizResultModel
from src.domain.services.quiz_base import BaseQuizManager, _normalize_text, _sha256
from src.core.settings import app_settings
from src.dal.remote.gemini import GeminiClient
from src.domain.models.input_model import InputModel
from src.domain.models.topics_model import TopicModel
from src.dal.remote.factory import AdapterFactory
from src.dal.local.db_adapter import DBAdapter
from src.core.logs import error, debug




class QuizAPIManager(BaseQuizManager):
    
    def __init__(self):
        super().__init__()
        
        self.adapters_factory = AdapterFactory()
        self.gemini_client = GeminiClient()


    def get_all_sources(self):
        
        db_sources = self.db_adapter.read_all("accredit_sourceitem")

        list_of_sources = []

        for s in db_sources:
            source_name = s.get("source_name", None)
            if source_name:
                list_of_sources.append(source_name)

        return list_of_sources
    
    def get_source(self, source_name):

        db_source = self.db_adapter.read_by_id(
            "accredit_sourceitem", 
            source_name, 
            id_column="source_name"
        )

        return db_source
    
    def get_item_preview(self, item_name):
        adapter = self.adapters_factory.get_adapter(item_name)
        if not adapter:
            return None
        preview = adapter.get_preview()
        return preview
    

    def get_all_items(self):
        db_items = self.db_adapter.read_all("accredit_sourceitem")

        list_of_items = []

        for i in db_items:
            item_name = i.get("item_name", None)
            if item_name:
                list_of_items.append(item_name)

        return list_of_items
    
    def get_item(self, item_name):
        
        db_item = self.db_adapter.read_by_id(
            "accredit_sourceitem", 
            item_name, 
            id_column="item_name"
        )

        return db_item
    
    def get_topics(
        self,
        item_name: str,
        *,
        page: int = 1,
        per_page: int = 45,
        **adapter_kwargs,   # adapter-specific knobs if needed (e.g., time_window, tagged)
    ) -> dict[str, any]:
        adapter = self.adapters_factory.get_adapter(item_name)
        
        if not adapter:
            error(f"No adapter found for source: {item_name}")
            return TopicModel(
                item_name=item_name,
                page=page,
                per_page=per_page,
                topics=[],
                has_more=False,
                updated_at=datetime.now(timezone.utc).isoformat(),
                source_name=None,
            ).to_dict()

        res = adapter.get_topics(page=page, per_page=per_page, **adapter_kwargs)

        return TopicModel(
            item_name=res.get("item_name", item_name),
            page=res.get("page", page),
            per_page=res.get("per_page", per_page),
            topics=res.get("topics", []),
            has_more=bool(res.get("has_more")),
            updated_at=res.get("updated_at", datetime.now(timezone.utc).isoformat()),
            source_name=res.get("source_name"),
        ).to_dict()
    
    def get_input(
        self,
        item_name: str,
        input_identification: str,
        **adapter_kwargs,
    ) -> dict[str, any]:
        adapter = self.adapters_factory.get_adapter(item_name)
        if not adapter:
            error(f"No adapter found for source: {item_name}")
            return InputModel(
                source_name=item_name,
                item_name=item_name,
                input_identification=input_identification,
                input_data=None,
                updated_at=datetime.now(timezone.utc).isoformat(),
            ).to_dict()

        # 1) find SourceItem row
        source_item_db = self.db_adapter.read_where_one(
            "accredit_sourceitem",
            {"item_name": item_name}
        )
        if not source_item_db:
            error(f"Source item not found in DB for item_name: {item_name}")
            return InputModel(
                source_name=item_name,
                item_name=item_name,
                input_identification=input_identification,
                input_data=None,
                updated_at=datetime.now(timezone.utc).isoformat(),
            ).to_dict()

        source_item_id = source_item_db["id"]
        source_name = source_item_db.get("source_name", item_name)
        item_name_from_db = source_item_db.get("item_name", item_name)

        # 2) try cache in accredit_input
        row = self.db_adapter.read_where_one(
            "accredit_input",
            {"source_item_id": source_item_id, "input_identification": input_identification}
        )
        if row:
            debug(f"Input found in DB for {item_name} - {input_identification}")
            return InputModel(
                source_name=source_name,
                item_name=item_name_from_db,
                input_identification=row.get("input_identification", input_identification),
                input_data=row.get("input_data", None),
                updated_at=row.get("updated_at", datetime.now(timezone.utc).isoformat()),
            ).to_dict()

        # 3) fetch from adapter and persist
        debug(f"Input NOT found in DB for {item_name} - {input_identification}. Fetching from source...")
        response_from_adapter = adapter.get_input(input_identification=input_identification, **adapter_kwargs)

        inserted_pk = self.db_adapter.insert_row("accredit_input", {
            "source_item_id": source_item_id,
            "input_identification": input_identification,
            "input_data": response_from_adapter.get("input_data", None),
            "updated_at": response_from_adapter.get("updated_at", datetime.now(timezone.utc).isoformat()),
        })

        return InputModel(
            source_name=source_name,
            item_name=item_name_from_db,
            input_identification=input_identification,
            input_data=response_from_adapter.get("input_data", None),
            updated_at=response_from_adapter.get("updated_at", datetime.now(timezone.utc).isoformat()),
        ).to_dict()
    
    def search(
    self,
    item_name: str,
    q: str,
    page: int = 1,
    per_page: int = 45,
    mode: str = "fulltext",          # fulltext | substring | fuzzy
    time_window: str | None = None,
    fill_page: bool = True,
    max_extra_pages: int = 2,
    ) -> dict[str, any]:
        adapter = self.adapters_factory.get_adapter(item_name)
        if not adapter:
            error(f"No adapter found for source: {item_name}")
            return TopicModel(
                item_name=item_name,
                page=page,
                per_page=per_page,
                topics=[],
                has_more=False,
                updated_at=datetime.now(timezone.utc).isoformat(),
                source_name=None,
            ).to_dict()

        try:
            # DELEGAR para o adapter (o adapter já aplica 'mode')
            res = adapter.search(
                q=q,
                page=page,
                per_page=per_page,
                mode=mode,                 # <-- antes estava faltando
                time_window=time_window,
                fill_page=fill_page,
                max_extra_pages=max_extra_pages,
            ) or {}
        except Exception as ex:
            error(f"Adapter search failed for {item_name}: {ex}")
            return TopicModel(
                item_name=item_name,
                page=page,
                per_page=per_page,
                topics=[],
                has_more=False,
                updated_at=datetime.now(timezone.utc).isoformat(),
                source_name=None,
            ).to_dict()

        # Normalizar o envelope — garantir chaves esperadas
        topics = res.get("topics", []) or []
        return {
            "item_name": res.get("item_name", item_name),
            "source_name": res.get("source_name"),
            "page": res.get("page", page),
            "per_page": res.get("per_page", per_page),
            "has_more": bool(res.get("has_more")),
            "updated_at": res.get("updated_at", datetime.now(timezone.utc).isoformat()),
            "topics": topics,
        }
    

        
    async def generate_context(
        self,

        item_name: str,
        input_data: dict[str, any],
        input_identification: str,
        amount_question: int,
        force_new_generation: bool = False,
        *args,
        **kwargs
    ) -> dict[str, any]:
        adapter = self.adapters_factory.get_adapter(item_name)
        if not adapter:
            error(f"No adapter found for source: {item_name}")
            return {"error": "No adapter found"}

        # Caminho rápido: buscar no DB, a menos que force_new_generation esteja True
        if not force_new_generation:
            cached = self.get_questions(item_name=item_name, input_identification=input_identification, amount_question=amount_question)
            if cached:
                debug(f"Questions found in DB for {item_name} - {input_identification}")
                return cached
            else:
                debug(f"Questions NOT found in DB for {item_name} - {input_identification}. Generating...")

        # Gera UMA vez (ou porque forçou, ou porque não há no DB)
        prompt = adapter.generate_context(input_data=input_data, amount_question=amount_question, *args, **kwargs)
        response = await self.gemini_client.generate_text(
            prompt=prompt,
            system_instruction=adapter.instructions(),
            response_mime_type="application/json",
            temperature=0.7,
        )
        return response

    
    def get_questions(
        self,
        item_name: str,
        amount_question: int,
        input_identification: str | None = None,
    ) -> dict[str, any] | None:

        if not input_identification:
            return None

        # 1) SourceItem
        source_item_db = self.db_adapter.read_where_one(
            "accredit_sourceitem",
            {"item_name": item_name}
        )
        if not source_item_db:
            error(f"Source item not found in DB for item_name: {item_name}")
            return None

        debug(f"Source item found in DB for item_name: {item_name}")

        # 2) Input
        input_db = self.db_adapter.read_where_one(
            "accredit_input",
            {"source_item_id": source_item_db["id"], "input_identification": input_identification}
        )
        if not input_db:
            debug(f"Input not cached yet for {item_name} / {input_identification}")
            return None
        
        debug(f"Input found in DB for {item_name} / {input_identification}")

        # 3) ALL questions for this input
        questions_db = self.db_adapter.read_where_many(
            "accredit_question",
            {"input_id": input_db["id"]},
            # You can add order_by here if your adapter prefers a stable order
        )

        debug(f"Fetched {len(questions_db)} questions from DB for {item_name} / {input_identification}")

        if not questions_db:
            debug(f"No questions in DB for {item_name} / {input_identification}")
            return None
        


        payload = {"questions": []}

        # 4) Load answers per question & assemble the public format
        for q in questions_db[:amount_question]:  # limit to amount_question
            # print override the same  line in terminal
            if app_settings().development_mode:
                print(f"Processing question ID {q['id']}", end='\r')

            answers = self.db_adapter.read_where_many(
                "accredit_answer",
                {"question_id": q["id"]},
                # optionally order_by by 'position'
            )
            if not answers:
                # no answers means this question isn't usable yet; skip
                continue

            options = [a["text"] for a in answers]
            correct = next((a["text"] for a in answers if a.get("is_correct") in (True, 1)), None)
            if not correct:
                # inconsistent data; skip
                continue

            payload["questions"].append({
                "question": q["question_text"],
                "correct_answer": correct,
                "options": options,
                "justification": q.get("justification"),
                "difficulty": q.get("difficulty"),
            })

        return payload if payload["questions"] else None

    
    def save_questions(
    self,
    response: dict[str, any],
    *,
    item_name: str,
    input_identification: str,
    ) -> QuizResultModel:
        """
        Persist Gemini questions for the given (item_name, input_identification).
        Skips exact duplicates by hash and near-duplicates by similarity rule (>=0.71).
        Returns summary: {"inserted": N, "skipped_exact": M, "skipped_similar": K}
        """

        items = response.get("questions", []) or []

        debug(f"Saving {len(items)} questions for {item_name} / {input_identification}")

        source_item_db = self.db_adapter.read_where_one(
            "accredit_sourceitem", {"item_name": item_name}
        )
        if not source_item_db:
            error(f"Source item not found in DB for item_name: {item_name}")
            raise ValueError("Source item must be cached before saving questions.")

        input_db = self.db_adapter.read_where_one(
            "accredit_input",
            {"source_item_id": source_item_db["id"], "input_identification": input_identification}
        )
        if not input_db:
            error(f"Input not found in DB for item_name: {item_name}, input_identification: {input_identification}")
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

            if not qtext or not correct or not options:
                error(f"Invalid question data (missing parts): {q}")
                continue

            debug(f"Normalizing question text for {item_name} / {input_identification}")

            norm = _normalize_text(qtext)
            nhash = _sha256(norm)

            # Exact duplicate by normalized hash?
            existing = self.db_adapter.read_where_one(
                "accredit_question",
                {"input_id": input_db["id"], "normalized_text_hash": nhash}
            )
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
            [question_id] = self.db_adapter.insert_row("accredit_question", {
                "input_id": input_db["id"],
                "question_text": qtext,
                "normalized_text": norm,
                "normalized_text_hash": nhash,
                "justification": justification,
                "difficulty": difficulty,
                "embedding": cand_vec,  # None is fine if you can't embed yet
                "updated_at": datetime.now(timezone.utc).isoformat(),
            })

            saved_questions.append({
                "id": question_id,
                "question_text": qtext,
            })

            # Insert Answers
            for idx, opt in enumerate(options):
                opt_text = (opt or "").strip()
                if not opt_text:
                    continue
                opt_norm = _normalize_text(opt_text)
                opt_hash = _sha256(opt_norm)
                is_correct = (opt_text == correct)

                self.db_adapter.insert_row("accredit_answer", {
                    "question_id": question_id,
                    "text": opt_text,
                    "normalized_text": opt_norm,
                    "normalized_text_hash": opt_hash,
                    "is_correct": 1 if is_correct else 0,
                    "position": idx,
                    "updated_at": datetime.now(timezone.utc).isoformat(),
                })

            inserted += 1

        return QuizResultModel(
            saved_questions=saved_questions,
            identification=f"{item_name}:{input_identification}",
            created_at=datetime.now(timezone.utc).isoformat(),
        )

   



            