-- SQLite test equivalent for 002_v2_studies_sources.sql (#35).
ALTER TABLE studies ADD COLUMN source_origin TEXT NOT NULL DEFAULT 'direct';
ALTER TABLE studies ADD COLUMN status TEXT NOT NULL DEFAULT 'draft';
ALTER TABLE studies ADD COLUMN quiz_id TEXT NOT NULL DEFAULT '';
ALTER TABLE studies ADD COLUMN expires_at BIGINT;

ALTER TABLE sources ADD COLUMN source_kind TEXT NOT NULL DEFAULT 'upload';
ALTER TABLE sources ADD COLUMN selection_kind TEXT NOT NULL DEFAULT 'all';
ALTER TABLE sources ADD COLUMN selection_spec TEXT NOT NULL DEFAULT '';
ALTER TABLE sources ADD COLUMN is_restricted INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sources ADD COLUMN external_ref TEXT NOT NULL DEFAULT '';

CREATE TABLE IF NOT EXISTS study_analysis (
    study_id TEXT PRIMARY KEY REFERENCES studies(id) ON DELETE CASCADE,
    total_sources INTEGER NOT NULL DEFAULT 0 CHECK(total_sources >= 0),
    total_bytes BIGINT NOT NULL DEFAULT 0 CHECK(total_bytes >= 0),
    ready_sources INTEGER NOT NULL DEFAULT 0 CHECK(ready_sources >= 0),
    updated_at BIGINT NOT NULL CHECK(updated_at >= 0)
);
