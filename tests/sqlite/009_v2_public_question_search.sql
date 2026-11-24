-- SQLite counterpart of the public prompt search projection.
CREATE TABLE IF NOT EXISTS public_question_search (
    question_version_id TEXT NOT NULL REFERENCES question_versions(id) ON DELETE CASCADE,
    quiz_id TEXT NOT NULL REFERENCES quizzes(id) ON DELETE CASCADE,
    normalized_prompt TEXT NOT NULL,
    normalized_title TEXT NOT NULL,
    normalized_tags TEXT NOT NULL,
    PRIMARY KEY (question_version_id, quiz_id)
);
CREATE INDEX IF NOT EXISTS public_question_search_by_quiz_version
    ON public_question_search(quiz_id, question_version_id);
