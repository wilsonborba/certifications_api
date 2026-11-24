-- Server-only source authorization provenance (#35). Legacy rows default to 'unknown'.
ALTER TABLE sources ADD COLUMN IF NOT EXISTS source_provenance TEXT NOT NULL DEFAULT 'unknown';
ALTER TABLE sources DROP CONSTRAINT IF EXISTS chk_sources_provenance;
ALTER TABLE sources ADD CONSTRAINT chk_sources_provenance CHECK (source_provenance IN ('unknown', 'upload', 'prepared_service', 'owned_ref'));
