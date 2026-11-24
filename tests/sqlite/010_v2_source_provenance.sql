ALTER TABLE sources ADD COLUMN source_provenance TEXT NOT NULL DEFAULT 'unknown' CHECK (source_provenance IN ('unknown', 'upload', 'prepared_service', 'owned_ref'));
