from contextlib import contextmanager

from sqlalchemy import (
    MetaData,
    Table,
    and_,
    create_engine,
    delete,
    insert,
    inspect,
    select,
    update,
)
from sqlalchemy.engine import Engine
from sqlalchemy.exc import NoSuchTableError

from src.core.logs import error
from src.core.settings import app_settings


class DBAdapter:
    """
    Database Adapter for local or dynamic DB operations.
    Provides engine access, safe connection context, reflection, and CRUD methods.
    """

    def __init__(self, engine: Engine = None):
        settings = app_settings()
        self.engine = engine or create_engine(settings.accredit_db.uri())
        try:
            from pgvector.sqlalchemy import Vector
            from sqlalchemy.dialects.postgresql import base as pg_base

            # Teach SQLAlchemy that the postgres type name "vector" maps to pgvector's Vector
            pg_base.ischema_names["vector"] = Vector
        except Exception as e:
            # If pgvector isn't installed or something odd happens, we just skip registration;
            # you'll still have the text-similarity fallback.
            error(f"pgvector import/registration failed: {e}")
            pass

    def get_engine(self) -> Engine:
        return self.engine

    @contextmanager
    def connect(self):
        conn = self.engine.connect()
        try:
            yield conn
        finally:
            conn.close()

    def get_inspector(self):
        return inspect(self.engine)

    def reflect_table(self, table_name: str, schema: str = None) -> Table:
        metadata = MetaData()
        try:
            return Table(table_name, metadata, autoload_with=self.engine, schema=schema)
        except NoSuchTableError:
            raise ValueError(f"Table '{table_name}' not found in the database.")

    # ------- CRUD operations -------- #

    def read_all(self, table_name: str, schema: str = None):
        table = self.reflect_table(table_name, schema)
        stmt = select(table)
        with self.connect() as conn:
            return [dict(row) for row in conn.execute(stmt).mappings()]

    def _build_conditions(self, table: Table, where: dict):
        """
        Supports:
          - equality: {"col": value}
          - range ops: {"col": {"$gte": x, "$lte": y, "$gt": x, "$lt": y}}
          - IN: {"col": {"$in": [a,b,c]}}
          - IS NULL: {"col": {"$isnull": True}}
        """
        conditions = []

        for key, value in where.items():
            if key not in table.c:
                raise ValueError(f"Unknown column '{key}' for table '{table.name}'")

            col = table.c[key]

            # Operator dict
            if isinstance(value, dict):
                for op, op_val in value.items():
                    if op == "$gte":
                        conditions.append(col >= op_val)
                    elif op == "$lte":
                        conditions.append(col <= op_val)
                    elif op == "$gt":
                        conditions.append(col > op_val)
                    elif op == "$lt":
                        conditions.append(col < op_val)
                    elif op == "$in":
                        if not isinstance(op_val, (list, tuple, set)):
                            raise ValueError(
                                f"$in for '{key}' must be a list/tuple/set"
                            )
                        conditions.append(col.in_(list(op_val)))
                    elif op == "$isnull":
                        conditions.append(col.is_(None) if op_val else col.is_not(None))
                    else:
                        raise ValueError(f"Unsupported operator '{op}' for '{key}'")

            # Equality
            else:
                conditions.append(col == value)

        return conditions

    def read_where_many(
        self,
        table_name: str,
        where: dict,
        *,
        limit: int | None = None,
        offset: int | None = None,
        order_by: list | None = None,
        schema: str = None,
    ):
        table = self.reflect_table(table_name, schema)

        conditions = self._build_conditions(table, where)
        stmt = select(table).where(and_(*conditions))

        if order_by:
            stmt = stmt.order_by(*order_by)
        if limit is not None:
            stmt = stmt.limit(limit)
        if offset is not None:
            stmt = stmt.offset(offset)

        with self.connect() as conn:
            return [dict(r) for r in conn.execute(stmt).mappings().all()]

    def read_where_one(self, table_name: str, where: dict, schema: str = None):
        """
        Return a single row as dict or None, matching all equality conditions in `where`.
        """
        table = self.reflect_table(table_name, schema)
        condition = and_(*[table.c[k] == v for k, v in where.items()])
        stmt = select(table).where(condition).limit(1)
        with self.connect() as conn:
            row = conn.execute(stmt).mappings().first()
            return dict(row) if row is not None else None

    def read_by_id(
        self, table_name: str, id_value, id_column: str = "id", schema: str = None
    ):
        table = self.reflect_table(table_name, schema)
        stmt = select(table).where(table.c[id_column] == id_value)
        with self.connect() as conn:
            row = conn.execute(stmt).mappings().first()
            return dict(row) if row is not None else None

    def insert_row(self, table_name: str, data: dict, schema: str = None):
        table = self.reflect_table(table_name, schema)
        stmt = insert(table).values(**data)
        with self.connect() as conn:
            result = conn.execute(stmt)
            conn.commit()
            pk = result.inserted_primary_key  # typically (123,)
            return pk[0] if pk else None

    def update_row(
        self,
        table_name: str,
        id_value,
        data: dict,
        id_column: str = "id",
        schema: str = None,
    ):
        table = self.reflect_table(table_name, schema)
        stmt = update(table).where(table.c[id_column] == id_value).values(**data)
        with self.connect() as conn:
            result = conn.execute(stmt)
            conn.commit()
            return result.rowcount

    def update_where(
        self, table_name: str, where: dict, data: dict, schema: str = None
    ):
        table = self.reflect_table(table_name, schema)
        condition = and_(*[table.c[k] == v for k, v in where.items()])
        stmt = update(table).where(condition).values(**data)
        with self.connect() as conn:
            result = conn.execute(stmt)
            conn.commit()
            return result.rowcount

    def delete_row(
        self, table_name: str, id_value, id_column: str = "id", schema: str = None
    ):
        table = self.reflect_table(table_name, schema)
        stmt = delete(table).where(table.c[id_column] == id_value)
        with self.connect() as conn:
            result = conn.execute(stmt)
            conn.commit()
            return result.rowcount

    def delete_where(self, table_name: str, where: dict, schema: str = None):
        table = self.reflect_table(table_name, schema)
        condition = and_(*[table.c[k] == v for k, v in where.items()])
        stmt = delete(table).where(condition)
        with self.connect() as conn:
            result = conn.execute(stmt)
            conn.commit()
            return result.rowcount

    # ------- Introspection (optional) ------- #

    def list_tables(self, schema: str = None):
        return self.get_inspector().get_table_names(schema=schema)

    def get_columns(self, table_name: str, schema: str = None):
        return self.get_inspector().get_columns(table_name, schema=schema)


# adapter = DBAdapter()

# # Read all users
# users = adapter.read_all("users")

# # Insert a row
# adapter.insert_row("users", {"name": "Alice", "email": "alice@example.com"})

# # Update a row
# adapter.update_row("users", id_value=1, data={"email": "new@example.com"})

# # Delete a row
# adapter.delete_row("users", id_value=1)

# # Schema inspection
# print(adapter.list_tables())
# print(adapter.get_columns("users"))
