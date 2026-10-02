"""Shared SQLAlchemy declarative base for security event ORM models."""

from sqlalchemy.orm import DeclarativeBase


class Base(DeclarativeBase):
    """Base class for SQLite ORM models."""
