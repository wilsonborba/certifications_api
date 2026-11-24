-- Preserve caller-selected Reports dataset identity across delayed generation jobs.
ALTER TABLE generation_job_sources ADD COLUMN IF NOT EXISTS external_ref TEXT NOT NULL DEFAULT '';
