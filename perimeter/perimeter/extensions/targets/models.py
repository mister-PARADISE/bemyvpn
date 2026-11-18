from __future__ import annotations

from sqlalchemy import JSON, Integer, String, Text
from sqlalchemy.orm import Mapped, mapped_column

from ...core.db import Base, TimestampMixin
from ...core.scope import Scope, display_host


class Target(TimestampMixin, Base):
    __tablename__ = "target"

    id: Mapped[int] = mapped_column(Integer, primary_key=True)
    domain: Mapped[str] = mapped_column(String(253), unique=True, nullable=False)
    title: Mapped[str] = mapped_column(String(160), default="", nullable=False)
    include: Mapped[list] = mapped_column(JSON, default=list, nullable=False)
    exclude: Mapped[list] = mapped_column(JSON, default=list, nullable=False)
    notes: Mapped[str] = mapped_column(Text, default="", nullable=False)

    @property
    def name(self) -> str:
        return display_host(self.domain)

    @property
    def scope(self) -> Scope:
        return Scope.build(self.domain, self.include or [], self.exclude or [])
