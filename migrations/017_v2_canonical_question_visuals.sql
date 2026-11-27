ALTER TABLE question_visuals ADD COLUMN IF NOT EXISTS owner_id TEXT NOT NULL DEFAULT '';
ALTER TABLE question_visuals ADD COLUMN IF NOT EXISTS study_id TEXT NOT NULL DEFAULT '';
ALTER TABLE question_visuals ADD COLUMN IF NOT EXISTS quiz_id TEXT NOT NULL DEFAULT '';
ALTER TABLE question_visuals ADD COLUMN IF NOT EXISTS question_id TEXT NOT NULL DEFAULT '';
ALTER TABLE question_visuals ADD COLUMN IF NOT EXISTS prepared_question_version_id TEXT NOT NULL DEFAULT '';
ALTER TABLE question_visuals ADD COLUMN IF NOT EXISTS result_question_version_id TEXT NOT NULL DEFAULT '';
ALTER TABLE question_visuals ADD COLUMN IF NOT EXISTS result_quiz_version_id TEXT NOT NULL DEFAULT '';
ALTER TABLE question_visual_jobs ADD COLUMN IF NOT EXISTS operation_fingerprint TEXT NOT NULL DEFAULT '';
ALTER TABLE question_visual_jobs ADD COLUMN IF NOT EXISTS result_question_version_id TEXT NOT NULL DEFAULT '';
ALTER TABLE question_visual_jobs ADD COLUMN IF NOT EXISTS result_quiz_version_id TEXT NOT NULL DEFAULT '';
ALTER TABLE question_visual_jobs ADD COLUMN IF NOT EXISTS result_document_id TEXT NOT NULL DEFAULT '';
ALTER TABLE question_visual_jobs ADD COLUMN IF NOT EXISTS result_fsm_ref_key TEXT NOT NULL DEFAULT '';

CREATE TABLE IF NOT EXISTS question_version_visual_refs (
    question_version_id TEXT NOT NULL REFERENCES question_versions(id) ON DELETE CASCADE,
    visual_id TEXT NOT NULL REFERENCES question_visuals(id) ON DELETE RESTRICT,
    PRIMARY KEY(question_version_id, visual_id)
);
CREATE INDEX IF NOT EXISTS question_version_visual_refs_by_visual ON question_version_visual_refs(visual_id);
