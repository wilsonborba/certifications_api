ALTER TABLE certificates ADD COLUMN IF NOT EXISTS holder_name_snapshot TEXT NOT NULL DEFAULT '';
ALTER TABLE certificates ADD COLUMN IF NOT EXISTS quiz_title_snapshot TEXT NOT NULL DEFAULT '';
ALTER TABLE certificates ADD COLUMN IF NOT EXISTS score_correct_snapshot INTEGER NOT NULL DEFAULT 0;
ALTER TABLE certificates ADD COLUMN IF NOT EXISTS score_total_snapshot INTEGER NOT NULL DEFAULT 0;
ALTER TABLE certificates ADD COLUMN IF NOT EXISTS pdf_fsm_ref TEXT NOT NULL DEFAULT '';
ALTER TABLE certificates ADD COLUMN IF NOT EXISTS image_fsm_ref TEXT NOT NULL DEFAULT '';
ALTER TABLE certificates ADD COLUMN IF NOT EXISTS revocation_reason TEXT NOT NULL DEFAULT '';
CREATE UNIQUE INDEX IF NOT EXISTS certificates_by_attempt ON certificates(attempt_id);
