from __future__ import annotations

from datetime import datetime

from sqlalchemy import JSON, DateTime, Integer, String, Text
from sqlalchemy.orm import Mapped, mapped_column

from ...core.db import Base

FEED_STATES = {
    "idle": ("ещё не обновлялись", "muted"),
    "running": ("обновляются", "accent"),
    "ok": ("свежие", "ok"),
    "failed": ("ошибка", "err"),
}


class FeedState(Base):
    """Как прошло последнее обновление данных модуля (core/feeds.py). Одна строка на источник."""

    __tablename__ = "feed_state"

    id: Mapped[str] = mapped_column(String(64), primary_key=True)
    status: Mapped[str] = mapped_column(String(16), default="idle", nullable=False)
    actor: Mapped[str] = mapped_column(String(64), default="", nullable=False)
    started_at: Mapped[datetime | None] = mapped_column(DateTime)
    finished_at: Mapped[datetime | None] = mapped_column(DateTime)
    ok_at: Mapped[datetime | None] = mapped_column(DateTime)  # последнее удачное: от него считается следующее
    summary: Mapped[str] = mapped_column(Text, default="", nullable=False)
    error: Mapped[str] = mapped_column(Text, default="", nullable=False)
    changed: Mapped[int] = mapped_column(Integer, default=0, nullable=False)
    items: Mapped[int] = mapped_column(Integer, default=0, nullable=False)
    count: Mapped[int] = mapped_column(Integer, default=0, nullable=False)  # сколько раз обновлялись
    rechecked: Mapped[int] = mapped_column(Integer, default=0, nullable=False)  # сколько целей перепроверили после
    log: Mapped[list] = mapped_column(JSON, default=list, nullable=False)  # [[время, уровень, текст], …]

    @property
    def state_title(self) -> str:
        return FEED_STATES.get(self.status, (self.status, ""))[0]

    @property
    def state_tone(self) -> str:
        return FEED_STATES.get(self.status, (self.status, ""))[1]
