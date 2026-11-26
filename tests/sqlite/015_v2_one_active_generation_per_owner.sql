-- SQLite equivalent of 015_v2_one_active_generation_per_owner.sql.
CREATE UNIQUE INDEX IF NOT EXISTS generation_jobs_one_active_per_owner
    ON generation_jobs(owner_id)
    WHERE status IN ('queued', 'running');
