ALTER TABLE questions ADD COLUMN IF NOT EXISTS origin TEXT NOT NULL DEFAULT 'supplied_by_creator' CHECK (origin IN ('generated_with_cortex', 'supplied_by_creator'));
ALTER TABLE quizzes ADD COLUMN IF NOT EXISTS origin TEXT NOT NULL DEFAULT 'supplied_by_creator' CHECK (origin IN ('generated_with_cortex', 'supplied_by_creator'));

CREATE TABLE IF NOT EXISTS question_visuals (
    id TEXT PRIMARY KEY CHECK(id <> ''),
    question_version_id TEXT NOT NULL REFERENCES question_versions(id) ON DELETE CASCADE,
    fsm_ref TEXT NOT NULL CHECK(fsm_ref <> ''),
    mime_type TEXT NOT NULL CHECK(mime_type <> ''),
    payload_sha256 TEXT NOT NULL DEFAULT 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855' CHECK(payload_sha256 <> ''),
    created_at BIGINT NOT NULL CHECK(created_at >= 0)
);
CREATE INDEX IF NOT EXISTS question_visuals_by_version ON question_visuals(question_version_id);

CREATE TABLE IF NOT EXISTS question_visual_jobs (
    id TEXT PRIMARY KEY CHECK(id <> ''),
    render_job_id TEXT NOT NULL UNIQUE CHECK(render_job_id <> ''),
    owner_id TEXT NOT NULL CHECK(owner_id <> ''),
    study_id TEXT NOT NULL REFERENCES studies(id) ON DELETE CASCADE,
    question_id TEXT NOT NULL REFERENCES questions(id) ON DELETE CASCADE,
    question_version_id TEXT NOT NULL REFERENCES question_versions(id) ON DELETE CASCADE,
    width BIGINT NOT NULL CHECK(width >= 200 AND width <= 2400),
    height BIGINT NOT NULL CHECK(height >= 200 AND height <= 2400),
    scale BIGINT NOT NULL CHECK(scale >= 1 AND scale <= 3),
    expires_at BIGINT NOT NULL CHECK(expires_at >= 0),
    cleanup_status TEXT NOT NULL DEFAULT 'pending' CHECK(cleanup_status IN ('pending', 'cleaned', 'failed')),
    cleanup_error TEXT NOT NULL DEFAULT '',
    created_at BIGINT NOT NULL CHECK(created_at >= 0)
);
CREATE INDEX IF NOT EXISTS question_visual_jobs_by_qv ON question_visual_jobs(question_version_id);
