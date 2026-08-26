"""SQLAlchemy metadata reserved for Certifications' durable domain records.

Redis remains the operational store for active studies, locks and transient
generation state.  Future completed certifications and audit records belong
here and must be introduced through an Alembic revision.
"""

from sqlalchemy.orm import DeclarativeBase


class Base(DeclarativeBase):
    """Base class for future PostgreSQL models owned by certifications_api."""

