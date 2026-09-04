"""SQLAlchemy metadata reserved for Certifications' durable domain records.

Redis remains the operational store for active studies, locks and transient
generation state. PostgreSQL persists durable user data, plan waitlist,
completed quizzes, share links, and attempts.
"""

import uuid
from datetime import datetime, timezone
from typing import Any

from sqlalchemy import (
    Boolean,
    DateTime,
    ForeignKey,
    Integer,
    Numeric,
    String,
    Text,
    UniqueConstraint,
    func,
    text,
)
from sqlalchemy.dialects.postgresql import JSONB, UUID
from sqlalchemy.orm import DeclarativeBase, Mapped, mapped_column, relationship


class Base(DeclarativeBase):
    """Base class for PostgreSQL models owned by certifications_api."""
    pass


class User(Base):
    __tablename__ = "users"

    id: Mapped[str] = mapped_column(UUID(as_uuid=False), primary_key=True)
    email: Mapped[str] = mapped_column(String(320), unique=True, nullable=False, index=True)
    metadata_json: Mapped[dict[str, Any] | None] = mapped_column(JSONB, nullable=True, server_default="{}")
    created_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), nullable=False, server_default=func.now()
    )
    updated_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), nullable=False, server_default=func.now(), onupdate=func.now()
    )

    waitlists: Mapped[list["PlanWaitlist"]] = relationship("PlanWaitlist", back_populates="user")
    quizzes: Mapped[list["CompletedQuiz"]] = relationship("CompletedQuiz", back_populates="owner")
    attempts: Mapped[list["QuizAttempt"]] = relationship("QuizAttempt", back_populates="user")


class PlanWaitlist(Base):
    __tablename__ = "plan_waitlist"

    id: Mapped[str] = mapped_column(
        UUID(as_uuid=False), primary_key=True, default=lambda: str(uuid.uuid4())
    )
    user_id: Mapped[str | None] = mapped_column(
        UUID(as_uuid=False), ForeignKey("users.id", ondelete="SET NULL"), nullable=True
    )
    email: Mapped[str] = mapped_column(String(320), nullable=False, index=True)
    plan: Mapped[str] = mapped_column(String(50), nullable=False)
    is_registered: Mapped[bool] = mapped_column(
        Boolean, nullable=False, default=False, server_default=text("false")
    )
    status: Mapped[str] = mapped_column(
        String(30), nullable=False, default="pending", server_default="pending"
    )
    ip_address: Mapped[str | None] = mapped_column(String(45), nullable=True)
    user_agent: Mapped[str | None] = mapped_column(Text, nullable=True)
    requested_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), nullable=False, server_default=func.now()
    )
    updated_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), nullable=False, server_default=func.now(), onupdate=func.now()
    )

    user: Mapped[User | None] = relationship("User", back_populates="waitlists")

    __table_args__ = (
        UniqueConstraint("email", "plan", name="uq_plan_waitlist_email_plan"),
    )


class CompletedQuiz(Base):
    __tablename__ = "completed_quizzes"

    id: Mapped[str] = mapped_column(
        UUID(as_uuid=False), primary_key=True, default=lambda: str(uuid.uuid4())
    )
    owner_id: Mapped[str] = mapped_column(
        UUID(as_uuid=False), ForeignKey("users.id", ondelete="CASCADE"), nullable=False
    )
    title: Mapped[str] = mapped_column(String(255), nullable=False)
    description: Mapped[str | None] = mapped_column(Text, nullable=True)
    visibility: Mapped[str] = mapped_column(
        String(20), nullable=False, default="private", server_default="private"
    )
    status: Mapped[str] = mapped_column(
        String(20), nullable=False, default="active", server_default="active"
    )
    total_questions: Mapped[int] = mapped_column(
        Integer, nullable=False, default=0, server_default=text("0")
    )
    total_attempts: Mapped[int] = mapped_column(
        Integer, nullable=False, default=0, server_default=text("0")
    )
    third_party_attempts: Mapped[int] = mapped_column(
        Integer, nullable=False, default=0, server_default=text("0")
    )
    quiz_data: Mapped[dict[str, Any]] = mapped_column(JSONB, nullable=False)
    created_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), nullable=False, server_default=func.now()
    )
    updated_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), nullable=False, server_default=func.now(), onupdate=func.now()
    )

    owner: Mapped[User] = relationship("User", back_populates="quizzes")
    shares: Mapped[list["QuizShare"]] = relationship("QuizShare", back_populates="quiz", cascade="all, delete-orphan")
    attempts: Mapped[list["QuizAttempt"]] = relationship("QuizAttempt", back_populates="quiz", cascade="all, delete-orphan")


class QuizShare(Base):
    __tablename__ = "quiz_shares"

    id: Mapped[str] = mapped_column(
        UUID(as_uuid=False), primary_key=True, default=lambda: str(uuid.uuid4())
    )
    quiz_id: Mapped[str] = mapped_column(
        UUID(as_uuid=False), ForeignKey("completed_quizzes.id", ondelete="CASCADE"), nullable=False
    )
    token: Mapped[str] = mapped_column(String(128), unique=True, nullable=False, index=True)
    expires_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), nullable=False)
    max_uses: Mapped[int | None] = mapped_column(Integer, nullable=True)
    current_uses: Mapped[int] = mapped_column(
        Integer, nullable=False, default=0, server_default=text("0")
    )
    is_active: Mapped[bool] = mapped_column(
        Boolean, nullable=False, default=True, server_default=text("true")
    )
    created_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), nullable=False, server_default=func.now()
    )

    quiz: Mapped[CompletedQuiz] = relationship("CompletedQuiz", back_populates="shares")


class QuizAttempt(Base):
    __tablename__ = "quiz_attempts"

    id: Mapped[str] = mapped_column(
        UUID(as_uuid=False), primary_key=True, default=lambda: str(uuid.uuid4())
    )
    quiz_id: Mapped[str] = mapped_column(
        UUID(as_uuid=False), ForeignKey("completed_quizzes.id", ondelete="CASCADE"), nullable=False
    )
    user_id: Mapped[str | None] = mapped_column(
        UUID(as_uuid=False), ForeignKey("users.id", ondelete="SET NULL"), nullable=True
    )
    user_name: Mapped[str | None] = mapped_column(String(255), nullable=True)
    score: Mapped[float] = mapped_column(Numeric(5, 2), nullable=False)
    correct_count: Mapped[int] = mapped_column(
        Integer, nullable=False, default=0, server_default=text("0")
    )
    wrong_count: Mapped[int] = mapped_column(
        Integer, nullable=False, default=0, server_default=text("0")
    )
    time_spent_seconds: Mapped[int] = mapped_column(
        Integer, nullable=False, default=0, server_default=text("0")
    )
    answers_json: Mapped[dict[str, Any] | None] = mapped_column(JSONB, nullable=True)
    completed_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), nullable=False, server_default=func.now()
    )

    quiz: Mapped[CompletedQuiz] = relationship("CompletedQuiz", back_populates="attempts")
    user: Mapped[User | None] = relationship("User", back_populates="attempts")


class UserGenerationLimit(Base):
    __tablename__ = "user_generation_limits"

    id: Mapped[str] = mapped_column(
        UUID(as_uuid=False), primary_key=True, default=lambda: str(uuid.uuid4())
    )
    user_id: Mapped[str | None] = mapped_column(
        UUID(as_uuid=False), ForeignKey("users.id", ondelete="CASCADE"), unique=True, nullable=True, index=True
    )
    user_email: Mapped[str | None] = mapped_column(String(320), unique=True, nullable=True, index=True)
    daily_limit: Mapped[int] = mapped_column(Integer, nullable=False, default=10, server_default=text("10"))
    created_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), nullable=False, server_default=func.now()
    )
    updated_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), nullable=False, server_default=func.now(), onupdate=func.now()
    )
