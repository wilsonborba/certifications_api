"""add_server_defaults_for_orm_columns

Revision ID: 5b407552786a
Revises: 6461241888b1
Create Date: 2026-08-30 12:17:30.825409

Adds DB-level (server_default) defaults matching the existing Python-side
(ORM) defaults on columns that previously only had the latter. Without a
server_default, a raw-SQL insert that skips these columns would violate
their NOT NULL constraints even though SQLAlchemy-mediated inserts always
worked fine because the ORM itself was filling the value in Python.

No column types, nullability, or data are changed: only server_default is
added, so this migration is safe to run against a live table with existing
rows.
"""

from collections.abc import Sequence

from alembic import op
import sqlalchemy as sa


# revision identifiers, used by Alembic.
revision: str = '5b407552786a'
down_revision: str | Sequence[str] | None = '6461241888b1'
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    # plan_waitlist
    op.alter_column(
        'plan_waitlist', 'is_registered',
        existing_type=sa.Boolean(),
        server_default=sa.text('false'),
        existing_nullable=False,
    )
    op.alter_column(
        'plan_waitlist', 'status',
        existing_type=sa.String(length=30),
        server_default='pending',
        existing_nullable=False,
    )

    # completed_quizzes
    op.alter_column(
        'completed_quizzes', 'visibility',
        existing_type=sa.String(length=20),
        server_default='private',
        existing_nullable=False,
    )
    op.alter_column(
        'completed_quizzes', 'status',
        existing_type=sa.String(length=20),
        server_default='active',
        existing_nullable=False,
    )
    op.alter_column(
        'completed_quizzes', 'total_questions',
        existing_type=sa.Integer(),
        server_default=sa.text('0'),
        existing_nullable=False,
    )
    op.alter_column(
        'completed_quizzes', 'total_attempts',
        existing_type=sa.Integer(),
        server_default=sa.text('0'),
        existing_nullable=False,
    )
    op.alter_column(
        'completed_quizzes', 'third_party_attempts',
        existing_type=sa.Integer(),
        server_default=sa.text('0'),
        existing_nullable=False,
    )

    # quiz_shares
    op.alter_column(
        'quiz_shares', 'current_uses',
        existing_type=sa.Integer(),
        server_default=sa.text('0'),
        existing_nullable=False,
    )
    op.alter_column(
        'quiz_shares', 'is_active',
        existing_type=sa.Boolean(),
        server_default=sa.text('true'),
        existing_nullable=False,
    )

    # quiz_attempts
    op.alter_column(
        'quiz_attempts', 'correct_count',
        existing_type=sa.Integer(),
        server_default=sa.text('0'),
        existing_nullable=False,
    )
    op.alter_column(
        'quiz_attempts', 'wrong_count',
        existing_type=sa.Integer(),
        server_default=sa.text('0'),
        existing_nullable=False,
    )
    op.alter_column(
        'quiz_attempts', 'time_spent_seconds',
        existing_type=sa.Integer(),
        server_default=sa.text('0'),
        existing_nullable=False,
    )


def downgrade() -> None:
    op.alter_column('quiz_attempts', 'time_spent_seconds', existing_type=sa.Integer(), server_default=None, existing_nullable=False)
    op.alter_column('quiz_attempts', 'wrong_count', existing_type=sa.Integer(), server_default=None, existing_nullable=False)
    op.alter_column('quiz_attempts', 'correct_count', existing_type=sa.Integer(), server_default=None, existing_nullable=False)

    op.alter_column('quiz_shares', 'is_active', existing_type=sa.Boolean(), server_default=None, existing_nullable=False)
    op.alter_column('quiz_shares', 'current_uses', existing_type=sa.Integer(), server_default=None, existing_nullable=False)

    op.alter_column('completed_quizzes', 'third_party_attempts', existing_type=sa.Integer(), server_default=None, existing_nullable=False)
    op.alter_column('completed_quizzes', 'total_attempts', existing_type=sa.Integer(), server_default=None, existing_nullable=False)
    op.alter_column('completed_quizzes', 'total_questions', existing_type=sa.Integer(), server_default=None, existing_nullable=False)
    op.alter_column('completed_quizzes', 'status', existing_type=sa.String(length=20), server_default=None, existing_nullable=False)
    op.alter_column('completed_quizzes', 'visibility', existing_type=sa.String(length=20), server_default=None, existing_nullable=False)

    op.alter_column('plan_waitlist', 'status', existing_type=sa.String(length=30), server_default=None, existing_nullable=False)
    op.alter_column('plan_waitlist', 'is_registered', existing_type=sa.Boolean(), server_default=None, existing_nullable=False)
