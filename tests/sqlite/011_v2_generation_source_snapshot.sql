-- SQLite equivalent of 011_v2_generation_source_snapshot.sql.
ALTER TABLE generation_job_sources ADD COLUMN filename TEXT NOT NULL DEFAULT '';
ALTER TABLE generation_job_sources ADD COLUMN mime_type TEXT NOT NULL DEFAULT '';
ALTER TABLE generation_job_sources ADD COLUMN size_bytes BIGINT NOT NULL DEFAULT 0;
ALTER TABLE generation_job_sources ADD COLUMN sha256 TEXT NOT NULL DEFAULT '';
ALTER TABLE generation_job_sources ADD COLUMN source_kind TEXT NOT NULL DEFAULT '';
ALTER TABLE generation_job_sources ADD COLUMN selection_kind TEXT NOT NULL DEFAULT 'all';
ALTER TABLE generation_job_sources ADD COLUMN selection_spec TEXT NOT NULL DEFAULT '';

ALTER TABLE generation_jobs ADD COLUMN cortex_session_id TEXT NOT NULL DEFAULT '';
ALTER TABLE generation_jobs ADD COLUMN cortex_output_id TEXT NOT NULL DEFAULT '';
