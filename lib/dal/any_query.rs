use sqlx::{Any, any::AnyArguments, query::Query};

/// A narrow SQLx `Any` query builder that emits portable numbered placeholders.
/// `QueryBuilder<Any>::push_bind` writes `?`, which PostgreSQL does not accept.
#[derive(Debug, Default)]
pub(crate) struct AnyQuery {
    sql: String,
    binds: Vec<BindValue>,
}

#[derive(Clone, Debug)]
enum BindValue {
    Text(String),
    I64(i64),
}

impl AnyQuery {
    pub(crate) fn new(sql: &str) -> Self {
        Self {
            sql: sql.to_owned(),
            binds: Vec::new(),
        }
    }

    pub(crate) fn push(&mut self, sql: &str) -> &mut Self {
        self.sql.push_str(sql);
        self
    }

    pub(crate) fn push_bind_text(&mut self, value: impl Into<String>) -> &mut Self {
        self.push_placeholder();
        self.binds.push(BindValue::Text(value.into()));
        self
    }

    pub(crate) fn push_bind_i64(&mut self, value: i64) -> &mut Self {
        self.push_placeholder();
        self.binds.push(BindValue::I64(value));
        self
    }

    pub(crate) fn query(&self) -> Query<'_, Any, AnyArguments<'_>> {
        let mut query = sqlx::query::<Any>(&self.sql);
        for bind in self.binds.iter().cloned() {
            query = match bind {
                BindValue::Text(value) => query.bind(value),
                BindValue::I64(value) => query.bind(value),
            };
        }
        query
    }

    fn push_placeholder(&mut self) {
        let next = self.binds.len() + 1;
        self.sql.push('$');
        self.sql.push_str(&next.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::AnyQuery;
    use sqlx::{AnyPool, Row, any::AnyPoolOptions};

    async fn verify_dynamic_filters(pool: &AnyPool) {
        sqlx::raw_sql(
            "CREATE TEMP TABLE placeholder_fixture (id TEXT, owner TEXT, created_at BIGINT);
             INSERT INTO placeholder_fixture VALUES
                 ('a', 'owner-1', 100), ('b', 'owner-1', 200), ('c', 'owner-2', 300);",
        )
        .execute(pool)
        .await
        .unwrap();

        let mut query = AnyQuery::new("SELECT id FROM placeholder_fixture WHERE owner = ");
        query
            .push_bind_text("owner-1")
            .push(" AND created_at >= ")
            .push_bind_i64(150)
            .push(" AND id IN (")
            .push_bind_text("b")
            .push(", ")
            .push_bind_text("c")
            .push(") ORDER BY id LIMIT ")
            .push_bind_i64(1)
            .push(" OFFSET ")
            .push_bind_i64(0);
        let rows = query.query().fetch_all(pool).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].try_get::<String, _>("id").unwrap(), "b");

        let empty = AnyQuery::new("SELECT COUNT(*) FROM placeholder_fixture");
        let count: i64 = empty
            .query()
            .fetch_one(pool)
            .await
            .unwrap()
            .try_get(0)
            .unwrap();
        assert_eq!(count, 3);
    }

    #[tokio::test]
    async fn numbered_placeholders_work_with_sqlite_any() {
        sqlx::any::install_default_drivers();
        let pool = AnyPoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        verify_dynamic_filters(&pool).await;
        pool.close().await;
    }
}
