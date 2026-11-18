from __future__ import annotations

from datetime import datetime
from urllib.parse import quote

from sqlalchemy import DateTime, Float, Integer, String, Text, UniqueConstraint
from sqlalchemy.orm import Mapped, mapped_column

from ...core.db import Base, TimestampMixin

STATES = {
    "unknown": ("не проверен", "neutral"),
    "checking": ("проверка", "accent"),
    "alive": ("жив", "success"),
    "dead": ("мёртв", "danger"),
}


class Proxy(TimestampMixin, Base):
    __tablename__ = "proxy"
    __table_args__ = (UniqueConstraint("scheme", "host", "port", "username", name="uq_proxy"),)

    id: Mapped[int] = mapped_column(Integer, primary_key=True)
    scheme: Mapped[str] = mapped_column(String(10), default="http", nullable=False)
    host: Mapped[str] = mapped_column(String(253), nullable=False)
    port: Mapped[int] = mapped_column(Integer, nullable=False)
    username: Mapped[str] = mapped_column(String(255), default="", nullable=False)
    password: Mapped[str] = mapped_column(String(255), default="", nullable=False)
    label: Mapped[str] = mapped_column(String(120), default="", nullable=False)
    status: Mapped[str] = mapped_column(String(10), default="unknown", nullable=False, index=True)
    latency_ms: Mapped[float | None] = mapped_column(Float)
    exit_ip: Mapped[str] = mapped_column(String(64), default="", nullable=False)
    country: Mapped[str] = mapped_column(String(64), default="", nullable=False)
    city: Mapped[str] = mapped_column(String(120), default="", nullable=False)
    org: Mapped[str] = mapped_column(String(200), default="", nullable=False)
    error: Mapped[str] = mapped_column(Text, default="", nullable=False)
    checked_at: Mapped[datetime | None] = mapped_column(DateTime)

    @property
    def hostport(self) -> str:
        host = f"[{self.host}]" if ":" in self.host else self.host
        return f"{host}:{self.port}"

    @property
    def url(self) -> str:
        """Адрес с логином и паролем — для подключения и копирования."""
        creds = ""
        if self.username:
            creds = quote(self.username, safe="") + (":" + quote(self.password, safe="") if self.password else "") + "@"
        return f"{self.scheme}://{creds}{self.hostport}"

    @property
    def display(self) -> str:
        """Без пароля — для экрана и журнала."""
        return f"{self.scheme}://{self.username + '@' if self.username else ''}{self.hostport}"

    @property
    def state_title(self) -> str:
        return STATES.get(self.status, (self.status, ""))[0]

    @property
    def state_tone(self) -> str:
        return STATES.get(self.status, (self.status, ""))[1]
