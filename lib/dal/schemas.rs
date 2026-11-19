use crate::core::{Error, Result, now};
use serde::{Deserialize, Serialize};
use sqlx::{AnyPool, Row};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SchemaField {
    pub field_path: String,
    pub field_type: String,
    pub is_required: bool,
    #[serde(default)]
    pub description: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DraftSchema {
    pub schema_name: String,
    pub version: i64,
    #[serde(default)]
    pub description: String,
    pub fields: Vec<SchemaField>,
}

#[derive(Clone, Debug)]
pub struct PublishedSchema {
    pub schema_name: String,
    pub version: i64,
    pub fields: Vec<SchemaField>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SchemaVersionDetail {
    pub schema_name: String,
    pub version: i64,
    pub status: String,
    pub description: String,
    pub created_at: i64,
    pub published_at: Option<i64>,
    pub fields: Vec<SchemaField>,
}

pub async fn save_draft(db: &AnyPool, draft: &DraftSchema) -> Result<()> {
    if draft.schema_name.trim().is_empty() || draft.version < 1 || draft.fields.is_empty() {
        return Err(Error::bad(
            "invalid_schema: Schema name, positive version, and fields are required",
        ));
    }
    let mut tx = db.begin().await?;
    let existing =
        sqlx::query("SELECT status FROM schema_versions WHERE schema_name = $1 AND version = $2")
            .bind(&draft.schema_name)
            .bind(draft.version)
            .fetch_optional(&mut *tx)
            .await?;

    if let Some(row) = existing {
        let status: String = row.get("status");
        if status != "draft" {
            return Err(Error::conflict(
                "schema_immutable: Published or retired schema versions cannot be modified",
            ));
        }
        sqlx::query(
            "UPDATE schema_versions SET description = $1 WHERE schema_name = $2 AND version = $3",
        )
        .bind(&draft.description)
        .bind(&draft.schema_name)
        .bind(draft.version)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM schema_fields WHERE schema_name = $1 AND version = $2")
            .bind(&draft.schema_name)
            .bind(draft.version)
            .execute(&mut *tx)
            .await?;
    } else {
        sqlx::query(
            "INSERT INTO schema_versions(schema_name, version, status, description, created_at) VALUES($1, $2, 'draft', $3, $4)",
        )
        .bind(&draft.schema_name)
        .bind(draft.version)
        .bind(&draft.description)
        .bind(now())
        .execute(&mut *tx)
        .await?;
    }

    for field in &draft.fields {
        sqlx::query(
            "INSERT INTO schema_fields(schema_name, version, field_path, field_type, is_required, description) VALUES($1, $2, $3, $4, $5, $6)",
        )
        .bind(&draft.schema_name)
        .bind(draft.version)
        .bind(&field.field_path)
        .bind(&field.field_type)
        .bind(i32::from(field.is_required))
        .bind(&field.description)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn publish(db: &AnyPool, schema_name: &str, version: i64) -> Result<()> {
    let row =
        sqlx::query("SELECT status FROM schema_versions WHERE schema_name = $1 AND version = $2")
            .bind(schema_name)
            .bind(version)
            .fetch_optional(db)
            .await?
            .ok_or_else(Error::missing)?;
    let status: String = row.get("status");
    if status == "retired" {
        return Err(Error::conflict(
            "schema_retired: Retired schema versions cannot be published",
        ));
    }
    if status == "published" {
        return Ok(());
    }
    sqlx::query(
        "UPDATE schema_versions SET status = 'published', published_at = $1 WHERE schema_name = $2 AND version = $3",
    )
    .bind(now())
    .bind(schema_name)
    .bind(version)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn retire(db: &AnyPool, schema_name: &str, version: i64) -> Result<()> {
    let row =
        sqlx::query("SELECT status FROM schema_versions WHERE schema_name = $1 AND version = $2")
            .bind(schema_name)
            .bind(version)
            .fetch_optional(db)
            .await?
            .ok_or_else(Error::missing)?;
    let status: String = row.get("status");
    if status == "draft" {
        return Err(Error::conflict(
            "schema_draft: Draft schema versions must be deleted instead of retired",
        ));
    }
    sqlx::query(
        "UPDATE schema_versions SET status = 'retired' WHERE schema_name = $1 AND version = $2",
    )
    .bind(schema_name)
    .bind(version)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn delete_draft(db: &AnyPool, schema_name: &str, version: i64) -> Result<()> {
    let row =
        sqlx::query("SELECT status FROM schema_versions WHERE schema_name = $1 AND version = $2")
            .bind(schema_name)
            .bind(version)
            .fetch_optional(db)
            .await?
            .ok_or_else(Error::missing)?;
    let status: String = row.get("status");
    if status != "draft" {
        return Err(Error::conflict(
            "schema_immutable: Only draft schema versions can be deleted",
        ));
    }
    sqlx::query("DELETE FROM schema_versions WHERE schema_name = $1 AND version = $2")
        .bind(schema_name)
        .bind(version)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn list_versions(db: &AnyPool, schema_name: &str) -> Result<Vec<SchemaVersionDetail>> {
    let rows = sqlx::query(
        "SELECT schema_name, version, status, description, created_at, published_at
         FROM schema_versions WHERE schema_name = $1 ORDER BY version DESC",
    )
    .bind(schema_name)
    .fetch_all(db)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let ver: i64 = r.get("version");
        let fields = load_fields(db, schema_name, ver).await?;
        out.push(SchemaVersionDetail {
            schema_name: r.get("schema_name"),
            version: ver,
            status: r.get("status"),
            description: r.get("description"),
            created_at: r.get("created_at"),
            published_at: r.try_get("published_at").ok(),
            fields,
        });
    }
    Ok(out)
}

pub async fn get_version(
    db: &AnyPool,
    schema_name: &str,
    version: i64,
) -> Result<SchemaVersionDetail> {
    let r = sqlx::query(
        "SELECT schema_name, version, status, description, created_at, published_at
         FROM schema_versions WHERE schema_name = $1 AND version = $2",
    )
    .bind(schema_name)
    .bind(version)
    .fetch_optional(db)
    .await?
    .ok_or_else(Error::missing)?;
    let fields = load_fields(db, schema_name, version).await?;
    Ok(SchemaVersionDetail {
        schema_name: r.get("schema_name"),
        version: r.get("version"),
        status: r.get("status"),
        description: r.get("description"),
        created_at: r.get("created_at"),
        published_at: r.try_get("published_at").ok(),
        fields,
    })
}

async fn load_fields(db: &AnyPool, schema_name: &str, version: i64) -> Result<Vec<SchemaField>> {
    let field_rows = sqlx::query(
        "SELECT field_path, field_type, is_required, description FROM schema_fields WHERE schema_name = $1 AND version = $2 ORDER BY field_path",
    )
    .bind(schema_name)
    .bind(version)
    .fetch_all(db)
    .await?;

    Ok(field_rows
        .into_iter()
        .map(|r| SchemaField {
            field_path: r.get("field_path"),
            field_type: r.get("field_type"),
            is_required: r.get::<i64, _>("is_required") != 0,
            description: r.get("description"),
        })
        .collect())
}

pub async fn get_published(
    db: &AnyPool,
    schema_name: &str,
    version: i64,
) -> Result<PublishedSchema> {
    let row =
        sqlx::query("SELECT status FROM schema_versions WHERE schema_name = $1 AND version = $2")
            .bind(schema_name)
            .bind(version)
            .fetch_optional(db)
            .await?
            .ok_or_else(|| {
                Error::unprocessable("schema_not_published: Schema version does not exist")
            })?;
    let status: String = row.get("status");
    if status != "published" {
        return Err(Error::unprocessable(
            "schema_not_published: Schema version is not published",
        ));
    }
    let fields = load_fields(db, schema_name, version).await?;

    Ok(PublishedSchema {
        schema_name: schema_name.to_string(),
        version,
        fields,
    })
}
