ALTER TABLE certificates ADD COLUMN holder_name_snapshot TEXT NOT NULL DEFAULT '';
ALTER TABLE certificates ADD COLUMN quiz_title_snapshot TEXT NOT NULL DEFAULT '';
ALTER TABLE certificates ADD COLUMN score_correct_snapshot INTEGER NOT NULL DEFAULT 0;
ALTER TABLE certificates ADD COLUMN score_total_snapshot INTEGER NOT NULL DEFAULT 0;
ALTER TABLE certificates ADD COLUMN pdf_fsm_ref TEXT NOT NULL DEFAULT '';
ALTER TABLE certificates ADD COLUMN image_fsm_ref TEXT NOT NULL DEFAULT '';
ALTER TABLE certificates ADD COLUMN revocation_reason TEXT NOT NULL DEFAULT '';
CREATE UNIQUE INDEX IF NOT EXISTS certificates_by_attempt ON certificates(attempt_id);
