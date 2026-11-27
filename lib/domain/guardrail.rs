use crate::{
    core::{Error, Result, now},
    dal::{
        Store, new_id,
        schemas::{self, PublishedSchema},
    },
};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

fn type_matches(expected: &str, value: &Value) -> bool {
    match expected {
        "string" => value.is_string(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "any" => true,
        _ => false,
    }
}

fn lookup_path<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = root;
    for part in path.split('.') {
        cur = cur.as_object()?.get(part)?;
    }
    Some(cur)
}

fn collect_leaf_paths(prefix: &str, value: &Value, out: &mut Vec<String>) {
    if let Some(obj) = value.as_object()
        && !obj.is_empty()
    {
        for (k, v) in obj {
            let next = if prefix.is_empty() {
                k.clone()
            } else {
                format!("{prefix}.{k}")
            };
            collect_leaf_paths(&next, v, out);
        }
        return;
    }
    if !prefix.is_empty() {
        out.push(prefix.to_string());
    }
}

pub fn validate(schema: &PublishedSchema, payload: &Value) -> Result<()> {
    if !payload.is_object() {
        return Err(Error::unprocessable(
            "validation_failed: Document payload must be a JSON object",
        ));
    }

    let mut declared: HashMap<&str, &str> = HashMap::new();
    let mut object_prefixes: HashSet<String> = HashSet::new();
    for f in &schema.fields {
        declared.insert(f.field_path.as_str(), f.field_type.as_str());
        if f.field_type == "any" || f.field_type == "object" {
            object_prefixes.insert(f.field_path.clone());
        }
        if f.is_required {
            let val = lookup_path(payload, &f.field_path).ok_or_else(|| {
                Error::unprocessable(format!(
                    "validation_failed: Missing required field '{}'",
                    f.field_path
                ))
            })?;
            if !type_matches(&f.field_type, val) {
                return Err(Error::unprocessable(format!(
                    "validation_failed: Field '{}' must be {}",
                    f.field_path, f.field_type
                )));
            }
        } else if let Some(val) = lookup_path(payload, &f.field_path)
            && !type_matches(&f.field_type, val)
        {
            return Err(Error::unprocessable(format!(
                "validation_failed: Field '{}' must be {}",
                f.field_path, f.field_type
            )));
        }
    }

    let mut leaf_paths = Vec::new();
    collect_leaf_paths("", payload, &mut leaf_paths);
    for path in leaf_paths {
        if declared.contains_key(path.as_str()) {
            continue;
        }
        let under_open_prefix = object_prefixes
            .iter()
            .any(|prefix| path.starts_with(&format!("{prefix}.")));
        if !under_open_prefix {
            return Err(Error::unprocessable(format!(
                "validation_failed: Undeclared field '{path}'"
            )));
        }
    }

    Ok(())
}

pub async fn store_validated_document(
    store: &Store,
    schema_name: &str,
    schema_version: i64,
    doc_id: &str,
    payload: &Value,
) -> Result<()> {
    let schema = schemas::get_published(&store.db, schema_name, schema_version).await?;
    validate(&schema, payload)?;
    store.docs.put_immutable(doc_id, payload).await
}

pub async fn cleanup_orphan_document(store: &Store, doc_id: &str) {
    let retained = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM question_versions WHERE couch_doc_id = $1",
    )
    .bind(doc_id)
    .fetch_one(&store.db)
    .await;
    match retained {
        Ok(0) => {}
        // Preserve documents when the database cannot prove that no version references them.
        _ => return,
    }
    if let Err(err) = store.docs.delete(doc_id).await {
        let ts = now();
        let _ = sqlx::query(
            "INSERT INTO cleanup_tasks(id, target_kind, target_ref, status, attempts, last_error, created_at, updated_at) VALUES($1, 'couch_doc', $2, 'pending', 1, $3, $4, $4)",
        )
        .bind(new_id())
        .bind(doc_id)
        .bind(err.to_string())
        .bind(ts)
        .execute(&store.db)
        .await;
    }
}
