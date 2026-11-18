CREATE TABLE IF NOT EXISTS attempt_activity_events (
    id TEXT PRIMARY KEY CHECK(id <> ''),
    attempt_id TEXT NOT NULL REFERENCES attempts(id) ON DELETE CASCADE,
    dedup_key TEXT NOT NULL CHECK(dedup_key <> ''),
    event_kind TEXT NOT NULL CHECK(event_kind IN ('focus_lost', 'page_hidden', 'page_return', 'away_duration', 'reconnect', 'refresh', 'gap_unknown')),
    duration_ms BIGINT NOT NULL DEFAULT 0 CHECK(duration_ms >= 0),
    occurred_at BIGINT NOT NULL CHECK(occurred_at >= 0),
    created_at BIGINT NOT NULL CHECK(created_at >= 0),
    UNIQUE (attempt_id, dedup_key)
);
CREATE INDEX IF NOT EXISTS attempt_activity_by_attempt ON attempt_activity_events(attempt_id, occurred_at);
