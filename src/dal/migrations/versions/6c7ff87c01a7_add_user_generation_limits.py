"""add_user_generation_limits

Revision ID: 6c7ff87c01a7
Revises: 5b407552786a
Create Date: 2026-09-09 00:00:00.000000

Adds the user_generation_limits table used by generation_policy_service.py
to grant a per-user custom daily generation quota, overriding the global
GENERATION_EASY_DAILY_LIMIT / GENERATION_PREMIUM_DAILY_LIMIT defaults.

This table already exists on the live database (created out of band via
manual SQL) with rows already in it. This migration only brings Alembic's
history in sync with that reality so a fresh environment creates the same
table; it must be applied to the live database via `alembic stamp
6c7ff87c01a7` rather than `alembic upgrade head`, since running the real
CREATE TABLE against a database that already has this table would fail.
"""

from collections.abc import Sequence

from alembic import op
import sqlalchemy as sa
from sqlalchemy.dialects import postgresql


# revision identifiers, used by Alembic.
revision: str = '6c7ff87c01a7'
down_revision: str | Sequence[str] | None = '5b407552786a'
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    op.create_table(
        'user_generation_limits',
        sa.Column('id', postgresql.UUID(as_uuid=False), nullable=False),
        sa.Column('user_id', postgresql.UUID(as_uuid=False), nullable=True),
        sa.Column('user_email', sa.String(length=320), nullable=True),
        sa.Column('daily_limit', sa.Integer(), nullable=False, server_default=sa.text('10')),
        sa.Column('created_at', sa.DateTime(timezone=True), nullable=False, server_default=sa.text('now()')),
        sa.Column('updated_at', sa.DateTime(timezone=True), nullable=False, server_default=sa.text('now()')),
        sa.ForeignKeyConstraint(['user_id'], ['users.id'], ondelete='CASCADE'),
        sa.PrimaryKeyConstraint('id'),
    )
    op.create_index(
        'ix_user_generation_limits_user_id', 'user_generation_limits', ['user_id'], unique=True
    )
    op.create_index(
        'ix_user_generation_limits_user_email', 'user_generation_limits', ['user_email'], unique=True
    )


def downgrade() -> None:
    op.drop_index('ix_user_generation_limits_user_email', table_name='user_generation_limits')
    op.drop_index('ix_user_generation_limits_user_id', table_name='user_generation_limits')
    op.drop_table('user_generation_limits')
