# Certifications PostgreSQL migrations

Alembic is prepared but no migration has been created or applied. The
database exists exclusively to reserve the durable storage boundary for future
completed certifications and relational history.

The active study workflow continues to use Redis for transient state and FSM
for files. Do not create tables manually. When a durable SQLAlchemy model is
approved, add it under `src/dal/local/` as a subclass of `Base`, review the
generated revision, and then run Alembic explicitly.

Useful validation command that does not apply a migration:

```bash
alembic current
```
