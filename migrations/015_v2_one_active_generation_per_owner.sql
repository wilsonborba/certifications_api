-- Enforce the existing one-active-generation-per-owner contract across concurrent requests.
CREATE UNIQUE INDEX IF NOT EXISTS generation_jobs_one_active_per_owner
    ON generation_jobs(owner_id)
    WHERE status IN ('queued', 'running');
