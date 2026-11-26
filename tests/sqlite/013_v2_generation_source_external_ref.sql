-- SQLite equivalent of 013_v2_generation_source_external_ref.sql.
ALTER TABLE generation_job_sources ADD COLUMN external_ref TEXT NOT NULL DEFAULT '';
