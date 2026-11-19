-- SQLite test equivalent for 003_v2_generation_events.sql (#38).
ALTER TABLE generation_jobs ADD COLUMN idempotency_key TEXT NOT NULL DEFAULT '';
ALTER TABLE generation_jobs ADD COLUMN queue_position INTEGER NOT NULL DEFAULT 0;
ALTER TABLE generation_jobs ADD COLUMN saved_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE generation_jobs ADD COLUMN stopped_reason TEXT NOT NULL DEFAULT '';

CREATE TABLE IF NOT EXISTS generation_events (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id TEXT NOT NULL REFERENCES generation_jobs(id) ON DELETE CASCADE,
    event_type TEXT NOT NULL CHECK(event_type <> ''),
    message TEXT NOT NULL DEFAULT '',
    created_at BIGINT NOT NULL CHECK(created_at >= 0)
);
CREATE INDEX IF NOT EXISTS generation_events_by_job ON generation_events(job_id, seq);
