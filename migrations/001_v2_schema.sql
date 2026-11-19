-- Certifications v2 normalized PostgreSQL schema (#32).
-- No JSON, JSONB, array, or nested-list columns; evolving documents live in CouchDB
-- validated against published schema_versions/schema_fields; file bytes live in FSM.

CREATE TABLE IF NOT EXISTS schema_versions (
    schema_name TEXT NOT NULL CHECK(schema_name <> ''),
    version INTEGER NOT NULL CHECK(version >= 1),
    status TEXT NOT NULL CHECK(status IN ('draft', 'published', 'retired')),
    description TEXT NOT NULL DEFAULT '',
    created_at BIGINT NOT NULL CHECK(created_at >= 0),
    published_at BIGINT,
    PRIMARY KEY (schema_name, version)
);

CREATE TABLE IF NOT EXISTS schema_fields (
    schema_name TEXT NOT NULL,
    version INTEGER NOT NULL,
    field_path TEXT NOT NULL CHECK(field_path <> ''),
    field_type TEXT NOT NULL CHECK(field_type IN ('string', 'integer', 'number', 'boolean', 'object', 'array', 'any')),
    is_required INTEGER NOT NULL CHECK(is_required IN (0, 1)),
    description TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (schema_name, version, field_path),
    FOREIGN KEY (schema_name, version) REFERENCES schema_versions(schema_name, version) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS runtime_settings (
    key TEXT PRIMARY KEY CHECK(key <> ''),
    value_int BIGINT,
    value_text TEXT,
    description TEXT NOT NULL DEFAULT '',
    updated_at BIGINT NOT NULL CHECK(updated_at >= 0),
    updated_by TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS studies (
    id TEXT PRIMARY KEY CHECK(id <> ''),
    owner_id TEXT NOT NULL CHECK(owner_id <> ''),
    title TEXT NOT NULL CHECK(title <> ''),
    description TEXT NOT NULL DEFAULT '',
    created_at BIGINT NOT NULL CHECK(created_at >= 0),
    updated_at BIGINT NOT NULL CHECK(updated_at >= 0)
);
CREATE INDEX IF NOT EXISTS studies_by_owner ON studies(owner_id, updated_at DESC);

CREATE TABLE IF NOT EXISTS tags (
    id TEXT PRIMARY KEY CHECK(id <> ''),
    slug TEXT NOT NULL UNIQUE CHECK(slug <> ''),
    label TEXT NOT NULL CHECK(label <> ''),
    created_at BIGINT NOT NULL CHECK(created_at >= 0)
);

CREATE TABLE IF NOT EXISTS study_tags (
    study_id TEXT NOT NULL REFERENCES studies(id) ON DELETE CASCADE,
    tag_id TEXT NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    PRIMARY KEY (study_id, tag_id)
);

CREATE TABLE IF NOT EXISTS sources (
    id TEXT PRIMARY KEY CHECK(id <> ''),
    owner_id TEXT NOT NULL CHECK(owner_id <> ''),
    study_id TEXT NOT NULL REFERENCES studies(id) ON DELETE CASCADE,
    filename TEXT NOT NULL CHECK(filename <> ''),
    mime_type TEXT NOT NULL CHECK(mime_type <> ''),
    size_bytes BIGINT NOT NULL CHECK(size_bytes >= 0),
    sha256 TEXT NOT NULL DEFAULT '',
    fsm_ref TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL CHECK(status IN ('reserved', 'uploading', 'ready', 'failed', 'deleted')),
    error_code TEXT NOT NULL DEFAULT '',
    created_at BIGINT NOT NULL CHECK(created_at >= 0),
    updated_at BIGINT NOT NULL CHECK(updated_at >= 0)
);
CREATE INDEX IF NOT EXISTS sources_by_owner_study ON sources(owner_id, study_id, created_at DESC);

CREATE TABLE IF NOT EXISTS source_ledger (
    id TEXT PRIMARY KEY CHECK(id <> ''),
    owner_id TEXT NOT NULL CHECK(owner_id <> ''),
    study_id TEXT NOT NULL REFERENCES studies(id) ON DELETE CASCADE,
    source_id TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    state TEXT NOT NULL CHECK(state IN ('reserved', 'committed', 'released', 'failed')),
    bytes BIGINT NOT NULL CHECK(bytes >= 0),
    created_at BIGINT NOT NULL CHECK(created_at >= 0),
    updated_at BIGINT NOT NULL CHECK(updated_at >= 0)
);
CREATE INDEX IF NOT EXISTS source_ledger_by_owner ON source_ledger(owner_id, state);

CREATE TABLE IF NOT EXISTS source_extractions (
    id TEXT PRIMARY KEY CHECK(id <> ''),
    source_id TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    status TEXT NOT NULL CHECK(status IN ('pending', 'processing', 'ready', 'failed')),
    schema_name TEXT NOT NULL,
    schema_version INTEGER NOT NULL,
    couch_doc_id TEXT NOT NULL DEFAULT '',
    error_code TEXT NOT NULL DEFAULT '',
    created_at BIGINT NOT NULL CHECK(created_at >= 0),
    updated_at BIGINT NOT NULL CHECK(updated_at >= 0),
    FOREIGN KEY (schema_name, schema_version) REFERENCES schema_versions(schema_name, version)
);

CREATE TABLE IF NOT EXISTS quizzes (
    id TEXT PRIMARY KEY CHECK(id <> ''),
    owner_id TEXT NOT NULL CHECK(owner_id <> ''),
    study_id TEXT REFERENCES studies(id) ON DELETE SET NULL,
    visibility TEXT NOT NULL CHECK(visibility IN ('private', 'public')),
    active_version_id TEXT NOT NULL DEFAULT '',
    created_at BIGINT NOT NULL CHECK(created_at >= 0),
    updated_at BIGINT NOT NULL CHECK(updated_at >= 0)
);
CREATE INDEX IF NOT EXISTS quizzes_by_owner ON quizzes(owner_id, updated_at DESC);

CREATE TABLE IF NOT EXISTS quiz_tags (
    quiz_id TEXT NOT NULL REFERENCES quizzes(id) ON DELETE CASCADE,
    tag_id TEXT NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    PRIMARY KEY (quiz_id, tag_id)
);

CREATE TABLE IF NOT EXISTS questions (
    id TEXT PRIMARY KEY CHECK(id <> ''),
    owner_id TEXT NOT NULL CHECK(owner_id <> ''),
    created_at BIGINT NOT NULL CHECK(created_at >= 0)
);
CREATE INDEX IF NOT EXISTS questions_by_owner ON questions(owner_id);

CREATE TABLE IF NOT EXISTS question_versions (
    id TEXT PRIMARY KEY CHECK(id <> ''),
    question_id TEXT NOT NULL REFERENCES questions(id) ON DELETE CASCADE,
    version_num INTEGER NOT NULL CHECK(version_num >= 1),
    language TEXT NOT NULL CHECK(language IN ('en', 'pt-BR', 'th')),
    schema_name TEXT NOT NULL,
    schema_version INTEGER NOT NULL,
    couch_doc_id TEXT NOT NULL CHECK(couch_doc_id <> ''),
    created_at BIGINT NOT NULL CHECK(created_at >= 0),
    UNIQUE (question_id, version_num),
    FOREIGN KEY (schema_name, schema_version) REFERENCES schema_versions(schema_name, version)
);

CREATE TABLE IF NOT EXISTS question_citations (
    id TEXT PRIMARY KEY CHECK(id <> ''),
    question_version_id TEXT NOT NULL REFERENCES question_versions(id) ON DELETE CASCADE,
    source_id TEXT NOT NULL REFERENCES sources(id) ON DELETE RESTRICT,
    page_or_offset TEXT NOT NULL DEFAULT '',
    created_at BIGINT NOT NULL CHECK(created_at >= 0)
);

CREATE TABLE IF NOT EXISTS quiz_versions (
    id TEXT PRIMARY KEY CHECK(id <> ''),
    quiz_id TEXT NOT NULL REFERENCES quizzes(id) ON DELETE CASCADE,
    version_num INTEGER NOT NULL CHECK(version_num >= 1),
    title TEXT NOT NULL CHECK(title <> ''),
    created_at BIGINT NOT NULL CHECK(created_at >= 0),
    UNIQUE (quiz_id, version_num)
);

CREATE TABLE IF NOT EXISTS quiz_version_questions (
    quiz_version_id TEXT NOT NULL REFERENCES quiz_versions(id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK(position >= 0),
    question_version_id TEXT NOT NULL REFERENCES question_versions(id) ON DELETE RESTRICT,
    PRIMARY KEY (quiz_version_id, position),
    UNIQUE (quiz_version_id, question_version_id)
);

CREATE TABLE IF NOT EXISTS generation_jobs (
    id TEXT PRIMARY KEY CHECK(id <> ''),
    owner_id TEXT NOT NULL CHECK(owner_id <> ''),
    study_id TEXT REFERENCES studies(id) ON DELETE SET NULL,
    quiz_id TEXT REFERENCES quizzes(id) ON DELETE SET NULL,
    status TEXT NOT NULL CHECK(status IN ('queued', 'running', 'completed', 'failed', 'cancelled')),
    language TEXT NOT NULL CHECK(language IN ('en', 'pt-BR', 'th')),
    requested_count INTEGER NOT NULL CHECK(requested_count BETWEEN 1 AND 20),
    cortex_job_id TEXT NOT NULL DEFAULT '',
    error_code TEXT NOT NULL DEFAULT '',
    queued_at BIGINT NOT NULL CHECK(queued_at >= 0),
    started_at BIGINT,
    finished_at BIGINT
);
CREATE INDEX IF NOT EXISTS generation_jobs_by_owner ON generation_jobs(owner_id, queued_at DESC);

CREATE TABLE IF NOT EXISTS generation_steps (
    id TEXT PRIMARY KEY CHECK(id <> ''),
    job_id TEXT NOT NULL REFERENCES generation_jobs(id) ON DELETE CASCADE,
    step_name TEXT NOT NULL CHECK(step_name <> ''),
    status TEXT NOT NULL CHECK(status IN ('queued', 'running', 'completed', 'failed', 'cancelled')),
    wait_ms BIGINT NOT NULL DEFAULT 0 CHECK(wait_ms >= 0),
    process_ms BIGINT NOT NULL DEFAULT 0 CHECK(process_ms >= 0),
    started_at BIGINT,
    finished_at BIGINT
);

CREATE TABLE IF NOT EXISTS generation_job_sources (
    job_id TEXT NOT NULL REFERENCES generation_jobs(id) ON DELETE CASCADE,
    source_id TEXT NOT NULL REFERENCES sources(id) ON DELETE RESTRICT,
    PRIMARY KEY (job_id, source_id)
);

CREATE TABLE IF NOT EXISTS attempts (
    id TEXT PRIMARY KEY CHECK(id <> ''),
    owner_id TEXT NOT NULL CHECK(owner_id <> ''),
    quiz_id TEXT NOT NULL REFERENCES quizzes(id) ON DELETE CASCADE,
    quiz_version_id TEXT NOT NULL REFERENCES quiz_versions(id) ON DELETE RESTRICT,
    status TEXT NOT NULL CHECK(status IN ('in_progress', 'completed', 'abandoned')),
    score_correct INTEGER NOT NULL DEFAULT 0 CHECK(score_correct >= 0),
    score_total INTEGER NOT NULL DEFAULT 0 CHECK(score_total >= 0),
    started_at BIGINT NOT NULL CHECK(started_at >= 0),
    completed_at BIGINT
);
CREATE INDEX IF NOT EXISTS attempts_by_owner ON attempts(owner_id, started_at DESC);

CREATE TABLE IF NOT EXISTS attempt_answers (
    id TEXT PRIMARY KEY CHECK(id <> ''),
    attempt_id TEXT NOT NULL REFERENCES attempts(id) ON DELETE CASCADE,
    question_version_id TEXT NOT NULL REFERENCES question_versions(id) ON DELETE RESTRICT,
    selected_option_id TEXT NOT NULL CHECK(selected_option_id <> ''),
    is_correct INTEGER NOT NULL CHECK(is_correct IN (0, 1)),
    answered_at BIGINT NOT NULL CHECK(answered_at >= 0),
    UNIQUE (attempt_id, question_version_id)
);

CREATE TABLE IF NOT EXISTS owner_activity (
    owner_id TEXT PRIMARY KEY CHECK(owner_id <> ''),
    total_attempts INTEGER NOT NULL DEFAULT 0 CHECK(total_attempts >= 0),
    completed_attempts INTEGER NOT NULL DEFAULT 0 CHECK(completed_attempts >= 0),
    total_correct INTEGER NOT NULL DEFAULT 0 CHECK(total_correct >= 0),
    total_answered INTEGER NOT NULL DEFAULT 0 CHECK(total_answered >= 0),
    updated_at BIGINT NOT NULL CHECK(updated_at >= 0)
);

CREATE TABLE IF NOT EXISTS attempt_statistics (
    id TEXT PRIMARY KEY CHECK(id <> ''),
    attempt_id TEXT NOT NULL REFERENCES attempts(id) ON DELETE CASCADE,
    schema_name TEXT NOT NULL,
    schema_version INTEGER NOT NULL,
    couch_doc_id TEXT NOT NULL CHECK(couch_doc_id <> ''),
    created_at BIGINT NOT NULL CHECK(created_at >= 0),
    FOREIGN KEY (schema_name, schema_version) REFERENCES schema_versions(schema_name, version)
);

CREATE TABLE IF NOT EXISTS estimation_statistics (
    id TEXT PRIMARY KEY CHECK(id <> ''),
    owner_id TEXT NOT NULL CHECK(owner_id <> ''),
    operation_kind TEXT NOT NULL CHECK(operation_kind <> ''),
    schema_name TEXT NOT NULL,
    schema_version INTEGER NOT NULL,
    couch_doc_id TEXT NOT NULL CHECK(couch_doc_id <> ''),
    created_at BIGINT NOT NULL CHECK(created_at >= 0),
    FOREIGN KEY (schema_name, schema_version) REFERENCES schema_versions(schema_name, version)
);

CREATE TABLE IF NOT EXISTS certificate_holders (
    id TEXT PRIMARY KEY CHECK(id <> ''),
    owner_id TEXT NOT NULL UNIQUE CHECK(owner_id <> ''),
    display_name TEXT NOT NULL CHECK(display_name <> ''),
    created_at BIGINT NOT NULL CHECK(created_at >= 0),
    updated_at BIGINT NOT NULL CHECK(updated_at >= 0)
);

CREATE TABLE IF NOT EXISTS certificates (
    id TEXT PRIMARY KEY CHECK(id <> ''),
    holder_id TEXT NOT NULL REFERENCES certificate_holders(id) ON DELETE RESTRICT,
    quiz_id TEXT NOT NULL REFERENCES quizzes(id) ON DELETE RESTRICT,
    attempt_id TEXT NOT NULL REFERENCES attempts(id) ON DELETE RESTRICT,
    verification_code TEXT NOT NULL UNIQUE CHECK(verification_code <> ''),
    status TEXT NOT NULL CHECK(status IN ('active', 'revoked')),
    issued_at BIGINT NOT NULL CHECK(issued_at >= 0),
    revoked_at BIGINT
);
CREATE INDEX IF NOT EXISTS certificates_by_holder ON certificates(holder_id, issued_at DESC);

CREATE TABLE IF NOT EXISTS cleanup_tasks (
    id TEXT PRIMARY KEY CHECK(id <> ''),
    target_kind TEXT NOT NULL CHECK(target_kind IN ('couch_doc', 'fsm_blob', 'source')),
    target_ref TEXT NOT NULL CHECK(target_ref <> ''),
    status TEXT NOT NULL CHECK(status IN ('pending', 'completed', 'failed')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0),
    last_error TEXT NOT NULL DEFAULT '',
    created_at BIGINT NOT NULL CHECK(created_at >= 0),
    updated_at BIGINT NOT NULL CHECK(updated_at >= 0)
);
CREATE INDEX IF NOT EXISTS cleanup_tasks_by_status ON cleanup_tasks(status, updated_at);
