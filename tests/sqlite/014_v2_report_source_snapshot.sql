-- SQLite equivalent of 014_v2_report_source_snapshot.sql.
ALTER TABLE sources ADD COLUMN report_calculation_id TEXT NOT NULL DEFAULT '';
ALTER TABLE sources ADD COLUMN report_calculation_version BIGINT NOT NULL DEFAULT 0;
ALTER TABLE sources ADD COLUMN report_row_count BIGINT NOT NULL DEFAULT 0;
ALTER TABLE sources ADD COLUMN report_lineage_sha256 TEXT NOT NULL DEFAULT '';
ALTER TABLE generation_job_sources ADD COLUMN report_calculation_id TEXT NOT NULL DEFAULT '';
ALTER TABLE generation_job_sources ADD COLUMN report_calculation_version BIGINT NOT NULL DEFAULT 0;
ALTER TABLE generation_job_sources ADD COLUMN report_row_count BIGINT NOT NULL DEFAULT 0;
ALTER TABLE generation_job_sources ADD COLUMN report_lineage_sha256 TEXT NOT NULL DEFAULT '';
