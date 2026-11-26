-- Freeze the source facts and selection used by each queued generation.
ALTER TABLE generation_job_sources ADD COLUMN IF NOT EXISTS filename TEXT NOT NULL DEFAULT '';
ALTER TABLE generation_job_sources ADD COLUMN IF NOT EXISTS mime_type TEXT NOT NULL DEFAULT '';
ALTER TABLE generation_job_sources ADD COLUMN IF NOT EXISTS size_bytes BIGINT NOT NULL DEFAULT 0;
ALTER TABLE generation_job_sources ADD COLUMN IF NOT EXISTS sha256 TEXT NOT NULL DEFAULT '';
ALTER TABLE generation_job_sources ADD COLUMN IF NOT EXISTS source_kind TEXT NOT NULL DEFAULT '';
ALTER TABLE generation_job_sources ADD COLUMN IF NOT EXISTS selection_kind TEXT NOT NULL DEFAULT 'all';
ALTER TABLE generation_job_sources ADD COLUMN IF NOT EXISTS selection_spec TEXT NOT NULL DEFAULT '';

ALTER TABLE generation_jobs ADD COLUMN IF NOT EXISTS cortex_session_id TEXT NOT NULL DEFAULT '';
ALTER TABLE generation_jobs ADD COLUMN IF NOT EXISTS cortex_output_id TEXT NOT NULL DEFAULT '';
