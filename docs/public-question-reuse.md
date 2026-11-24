# Public question catalog and reuse

## Discover reusable questions

`GET /catalog/questions` is public and does not require authentication.

Query parameters:

| Name | Meaning |
| --- | --- |
| `q` | Case-insensitive substring search across the question prompt and its current public source quiz title and tags. |
| `language` | Exact question language filter. |
| `origin` | Question origin filter. |
| `limit` | Page size, default 20, clamped to 1–50. |
| `offset` | Number of matching distinct immutable question versions to skip; defaults to 0. |

The response contains `items`, `limit`, `offset`, and `has_more`. Each item contains the canonical question `id`, immutable `question_version_id`, `source_quiz_title`, `source_quiz_tags`, `language`, `origin`, `prompt`, and `alternatives`. The catalog excludes private quizzes and never returns answer keys, explanations, citations, or source IDs. A version is eligible only while it belongs to an active version of a public quiz. Reused public questions must also retain verifiable origin membership.

## Reuse selected versions in a study

`POST /studies/{study_id}/questions/reuse` requires an authenticated study owner. The request body accepts only an explicit list of 1–50 immutable question version IDs:

```json
{
  "question_version_ids": ["<question-version-id>"]
}
```

Unknown fields are rejected with `422`; callers cannot supply ownership, visibility, source scope, or origin. The service revalidates every requested version against current public active-quiz membership. The operation uses one database transaction for eligibility reads and target quiz/version writes. A private, stale, or otherwise ineligible version rejects the complete selection without changes.

Success (`200`) returns:

```json
{
  "quiz_id": "<study-quiz-id>",
  "quiz_version_id": "<new-active-version-id>",
  "added_count": 1,
  "already_present_count": 0,
  "total_question_count": 1
}
```

Retrying an already-applied selection returns `added_count: 0`, preserves the active quiz version, and reports the existing membership in `already_present_count`. Reuse links the original canonical question and immutable version; it does not copy question payloads or citations. The server records `reused_public_question_version` plus the exact origin quiz and version. Existing generated and manual memberships remain unchanged.

Expected errors are `403` when the caller does not own the study, `404` when the study does not exist, `422` for invalid selection or a version not currently eligible, and `503` when the operation cannot be completed. A failed write rolls back the target quiz/version changes.

`GET /studies/{study_id}/questions` exposes each membership's `source_scope`, `origin_quiz_id`, and `origin_quiz_version_id` to the study owner. Reused source citations and visual storage references are omitted from this response. Reused canonical versions are immutable through study PATCH and visual creation operations (`409`). Public quiz detail also omits reused study-material citations. The attempt-taking API continues to omit answer keys.
