-- Prepared study generation configuration persistence (#35).
-- Stores bounded, normalized language and count choices for draft studies.
CREATE TABLE IF NOT EXISTS study_configs (
    study_id TEXT PRIMARY KEY REFERENCES studies(id) ON DELETE CASCADE,
    language TEXT NOT NULL CHECK(language IN ('en', 'pt-BR', 'th')),
    count INTEGER NOT NULL CHECK(count >= 1 AND count <= 20),
    created_at BIGINT NOT NULL CHECK(created_at >= 0),
    updated_at BIGINT NOT NULL CHECK(updated_at >= 0)
);
CREATE INDEX IF NOT EXISTS study_configs_by_study ON study_configs(study_id);
