from __future__ import annotations

from datetime import datetime
from typing import Any

from sqlalchemy import JSON, DateTime, ForeignKey, Index, Integer, String, Text, UniqueConstraint
from sqlalchemy.orm import Mapped, mapped_column

from ...core.db import Base, utcnow

RUN_STATES = {
    "queued": ("в очереди", "info"),
    "running": ("идёт", "accent"),
    "done": ("готово", "success"),
    "failed": ("ошибка", "danger"),
    "cancelled": ("остановлен", "warning"),
    "interrupted": ("прерван", "warning"),
    "skipped": ("пропущен", "warning"),  # не отработал модуль, на котором этот построен
}
ACTIVE_STATES = ("queued", "running")


class Run(Base):
    """Один запуск модуля по одной цели."""

    __tablename__ = "run"

    id: Mapped[int] = mapped_column(Integer, primary_key=True)
    target_id: Mapped[int] = mapped_column(ForeignKey("target.id", ondelete="CASCADE"), nullable=False, index=True)
    module_id: Mapped[str] = mapped_column(String(64), nullable=False, index=True)
    status: Mapped[str] = mapped_column(String(16), default="queued", nullable=False, index=True)
    params: Mapped[dict] = mapped_column(JSON, default=dict, nullable=False)
    actor: Mapped[str] = mapped_column(String(64), default="system", nullable=False)
    created_at: Mapped[datetime] = mapped_column(DateTime, default=utcnow, nullable=False)
    started_at: Mapped[datetime | None] = mapped_column(DateTime)
    finished_at: Mapped[datetime | None] = mapped_column(DateTime)
    progress: Mapped[dict | None] = mapped_column(JSON)
    summary: Mapped[str] = mapped_column(Text, default="", nullable=False)
    error: Mapped[str] = mapped_column(Text, default="", nullable=False)
    found_new: Mapped[int] = mapped_column(Integer, default=0, nullable=False)
    found_total: Mapped[int] = mapped_column(Integer, default=0, nullable=False)
    # цепочка: запуски одного нажатия («Запустить всё» или модуль со своими зависимостями) — id первого из них
    chain: Mapped[int | None] = mapped_column(Integer, index=True)

    @property
    def state_title(self) -> str:
        return RUN_STATES.get(self.status, (self.status, ""))[0]

    @property
    def state_tone(self) -> str:
        return RUN_STATES.get(self.status, (self.status, ""))[1]

    @property
    def active(self) -> bool:
        return self.status in ACTIVE_STATES

    @property
    def seconds(self) -> float | None:
        if self.started_at is None:
            return None
        end = self.finished_at or utcnow()
        return (end - self.started_at).total_seconds()


class RunLine(Base):
    """Строка вывода запуска — то, что видно в консоли."""

    __tablename__ = "run_line"
    __table_args__ = (Index("ix_run_line_run_n", "run_id", "n"),)

    id: Mapped[int] = mapped_column(Integer, primary_key=True)
    run_id: Mapped[int] = mapped_column(ForeignKey("run.id", ondelete="CASCADE"), nullable=False)
    n: Mapped[int] = mapped_column(Integer, nullable=False)
    at: Mapped[datetime] = mapped_column(DateTime, default=utcnow, nullable=False)
    level: Mapped[str] = mapped_column(String(8), default="info", nullable=False)
    text: Mapped[str] = mapped_column(Text, nullable=False)


class Asset(Base):
    """Находка внутри цели: IP, порт, DNS-запись, сосед по сети… Одна строка на (цель, вид, значение)."""

    __tablename__ = "asset"
    __table_args__ = (UniqueConstraint("target_id", "kind", "value", name="uq_asset"),
                      Index("ix_asset_target_kind", "target_id", "kind"))

    id: Mapped[int] = mapped_column(Integer, primary_key=True)
    target_id: Mapped[int] = mapped_column(ForeignKey("target.id", ondelete="CASCADE"), nullable=False)
    kind: Mapped[str] = mapped_column(String(32), nullable=False)
    value: Mapped[str] = mapped_column(String(512), nullable=False)
    data: Mapped[dict[str, Any]] = mapped_column(JSON, default=dict, nullable=False)
    source: Mapped[str] = mapped_column(String(64), default="", nullable=False)
    run_id: Mapped[int | None] = mapped_column(Integer)
    first_seen: Mapped[datetime] = mapped_column(DateTime, default=utcnow, nullable=False)
    last_seen: Mapped[datetime] = mapped_column(DateTime, default=utcnow, nullable=False)
