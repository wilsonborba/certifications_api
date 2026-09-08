"""drop_user_generation_limits

Revision ID: b73f105affdd
Revises: 6c7ff87c01a7
Create Date: 2026-09-09 00:00:00.000000

Removes the daily generation quota system entirely: this MVP has no daily
usage cap on question generation anymore (only the concurrency locks in
generation_policy_service.py remain, to serialize work per user and cap
global concurrent generations). user_generation_limits is dropped since it
only ever backed the per-user quota override, which no longer exists.
"""

from collections.abc import Sequence

from alembic import op
import sqlalchemy as sa
from sqlalchemy.dialects import postgresql


# revision identifiers, used by Alembic.
revision: str = 'b73f105affdd'
down_revision: str | Sequence[str] | None = '6c7ff87c01a7'
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    op.drop_index('ix_user_generation_limits_user_email', table_name='user_generation_limits')
    op.drop_index('ix_user_generation_limits_user_id', table_name='user_generation_limits')
    op.drop_table('user_generation_limits')


def downgrade() -> None:
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
