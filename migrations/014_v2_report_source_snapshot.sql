-- Persist bounded scalar lineage identity for a selected Reports dataset.
ALTER TABLE sources ADD COLUMN IF NOT EXISTS report_calculation_id TEXT NOT NULL DEFAULT '';
ALTER TABLE sources ADD COLUMN IF NOT EXISTS report_calculation_version BIGINT NOT NULL DEFAULT 0;
ALTER TABLE sources ADD COLUMN IF NOT EXISTS report_row_count BIGINT NOT NULL DEFAULT 0;
ALTER TABLE sources ADD COLUMN IF NOT EXISTS report_lineage_sha256 TEXT NOT NULL DEFAULT '';

ALTER TABLE generation_job_sources ADD COLUMN IF NOT EXISTS report_calculation_id TEXT NOT NULL DEFAULT '';
ALTER TABLE generation_job_sources ADD COLUMN IF NOT EXISTS report_calculation_version BIGINT NOT NULL DEFAULT 0;
ALTER TABLE generation_job_sources ADD COLUMN IF NOT EXISTS report_row_count BIGINT NOT NULL DEFAULT 0;
ALTER TABLE generation_job_sources ADD COLUMN IF NOT EXISTS report_lineage_sha256 TEXT NOT NULL DEFAULT '';
