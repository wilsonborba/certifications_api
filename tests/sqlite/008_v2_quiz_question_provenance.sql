ALTER TABLE quiz_version_questions ADD COLUMN source_scope TEXT NOT NULL DEFAULT 'unclassified' CHECK (source_scope IN ('generated_from_current_selected_sources', 'reused_public_question_version', 'unclassified'));

ALTER TABLE quiz_version_questions ADD COLUMN origin_quiz_id TEXT;
ALTER TABLE quiz_version_questions ADD COLUMN origin_quiz_version_id TEXT;

CREATE INDEX IF NOT EXISTS idx_quiz_version_questions_source_scope
    ON quiz_version_questions(quiz_version_id, source_scope);
