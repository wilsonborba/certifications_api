-- Enforce the existing one-active-generation-per-owner contract across concurrent requests.
CREATE UNIQUE INDEX IF NOT EXISTS generation_jobs_one_active_per_owner
    ON generation_jobs(owner_id)
    WHERE status IN ('queued', 'running');

-- A terminal job remains the durable response for its idempotency key. Keep
-- keys scoped to the same owner and study even after the active-job slot opens.
CREATE UNIQUE INDEX IF NOT EXISTS generation_jobs_owner_study_idempotency
    ON generation_jobs(owner_id, study_id, idempotency_key)
    WHERE idempotency_key <> '';
