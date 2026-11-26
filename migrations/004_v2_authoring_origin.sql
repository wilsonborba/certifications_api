ALTER TABLE questions ADD COLUMN IF NOT EXISTS origin TEXT NOT NULL DEFAULT 'supplied_by_creator' CHECK (origin IN ('generated_with_cortex', 'supplied_by_creator'));
ALTER TABLE quizzes ADD COLUMN IF NOT EXISTS origin TEXT NOT NULL DEFAULT 'supplied_by_creator' CHECK (origin IN ('generated_with_cortex', 'supplied_by_creator'));

CREATE TABLE IF NOT EXISTS question_visuals (
    id TEXT PRIMARY KEY CHECK(id <> ''),
    question_version_id TEXT NOT NULL REFERENCES question_versions(id) ON DELETE CASCADE,
    fsm_ref TEXT NOT NULL CHECK(fsm_ref <> ''),
    mime_type TEXT NOT NULL CHECK(mime_type <> ''),
    created_at BIGINT NOT NULL CHECK(created_at >= 0)
);
CREATE INDEX IF NOT EXISTS question_visuals_by_version ON question_visuals(question_version_id);
