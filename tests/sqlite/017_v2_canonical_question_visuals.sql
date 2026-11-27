ALTER TABLE question_visuals ADD COLUMN owner_id TEXT NOT NULL DEFAULT '';
ALTER TABLE question_visuals ADD COLUMN study_id TEXT NOT NULL DEFAULT '';
ALTER TABLE question_visuals ADD COLUMN quiz_id TEXT NOT NULL DEFAULT '';
ALTER TABLE question_visuals ADD COLUMN question_id TEXT NOT NULL DEFAULT '';
ALTER TABLE question_visuals ADD COLUMN prepared_question_version_id TEXT NOT NULL DEFAULT '';
ALTER TABLE question_visuals ADD COLUMN result_question_version_id TEXT NOT NULL DEFAULT '';
ALTER TABLE question_visuals ADD COLUMN result_quiz_version_id TEXT NOT NULL DEFAULT '';
ALTER TABLE question_visual_jobs ADD COLUMN operation_fingerprint TEXT NOT NULL DEFAULT '';
ALTER TABLE question_visual_jobs ADD COLUMN result_question_version_id TEXT NOT NULL DEFAULT '';
ALTER TABLE question_visual_jobs ADD COLUMN result_quiz_version_id TEXT NOT NULL DEFAULT '';
ALTER TABLE question_visual_jobs ADD COLUMN result_document_id TEXT NOT NULL DEFAULT '';
ALTER TABLE question_visual_jobs ADD COLUMN result_fsm_ref_key TEXT NOT NULL DEFAULT '';

CREATE TABLE IF NOT EXISTS question_version_visual_refs (
    question_version_id TEXT NOT NULL REFERENCES question_versions(id) ON DELETE CASCADE,
    visual_id TEXT NOT NULL REFERENCES question_visuals(id) ON DELETE RESTRICT,
    PRIMARY KEY(question_version_id, visual_id)
);
CREATE INDEX IF NOT EXISTS question_version_visual_refs_by_visual ON question_version_visual_refs(visual_id);
