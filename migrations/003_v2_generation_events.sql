-- Durable generation queue metadata and SSE event replay (#38).
ALTER TABLE generation_jobs ADD COLUMN IF NOT EXISTS idempotency_key TEXT NOT NULL DEFAULT '';
ALTER TABLE generation_jobs ADD COLUMN IF NOT EXISTS queue_position INTEGER NOT NULL DEFAULT 0;
ALTER TABLE generation_jobs ADD COLUMN IF NOT EXISTS saved_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE generation_jobs ADD COLUMN IF NOT EXISTS stopped_reason TEXT NOT NULL DEFAULT '';

CREATE TABLE IF NOT EXISTS generation_events (
    seq BIGSERIAL PRIMARY KEY,
    job_id TEXT NOT NULL REFERENCES generation_jobs(id) ON DELETE CASCADE,
    event_type TEXT NOT NULL CHECK(event_type <> ''),
    message TEXT NOT NULL DEFAULT '',
    created_at BIGINT NOT NULL CHECK(created_at >= 0)
);
CREATE INDEX IF NOT EXISTS generation_events_by_job ON generation_events(job_id, seq);
