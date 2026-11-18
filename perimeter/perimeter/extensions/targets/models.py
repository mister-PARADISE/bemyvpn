from __future__ import annotations

import secrets
from datetime import date, datetime

from sqlalchemy import JSON, Date, DateTime, Integer, String, Text
from sqlalchemy.orm import Mapped, mapped_column

from ...core.db import Base, TimestampMixin
from ...core.scope import Scope, display_host

AUTH_METHODS = {
    "dns_txt": "TXT-запись в DNS",
    "http_file": "Файл на сайте",
    "meta_tag": "Meta-тег на главной",
    "document": "Договор или подписанное письмо",
    "email": "Письмо с почты на этом домене",
}

AUTH_STATES = {
    "pending": ("Ждёт разрешения", "warning"),
    "verified": ("Разрешено", "success"),
    "expired": ("Разрешение истекло", "danger"),
    "revoked": ("Разрешение отозвано", "neutral"),
}


def new_auth_token() -> str:
    return secrets.token_hex(12)


class Target(TimestampMixin, Base):
    __tablename__ = "target"

    id: Mapped[int] = mapped_column(Integer, primary_key=True)
    domain: Mapped[str] = mapped_column(String(253), unique=True, nullable=False)
    title: Mapped[str] = mapped_column(String(160), default="", nullable=False)
    include: Mapped[list] = mapped_column(JSON, default=list, nullable=False)
    exclude: Mapped[list] = mapped_column(JSON, default=list, nullable=False)
    notes: Mapped[str] = mapped_column(Text, default="", nullable=False)

    auth_status: Mapped[str] = mapped_column(String(16), default="pending", nullable=False, index=True)
    auth_method: Mapped[str | None] = mapped_column(String(16))
    auth_token: Mapped[str] = mapped_column(String(64), default=new_auth_token, nullable=False)
    auth_note: Mapped[str] = mapped_column(Text, default="", nullable=False)
    auth_verified_at: Mapped[datetime | None] = mapped_column(DateTime)
    auth_valid_until: Mapped[date | None] = mapped_column(Date)

    @property
    def name(self) -> str:
        return display_host(self.domain)

    @property
    def scope(self) -> Scope:
        return Scope.build(self.domain, self.include or [], self.exclude or [])

    @property
    def auth_state(self) -> str:
        if self.auth_status == "verified" and self.auth_valid_until and self.auth_valid_until < date.today():
            return "expired"
        return self.auth_status

    @property
    def is_authorized(self) -> bool:
        """Можно ли запускать активные модули: разрешение есть и не истекло."""
        return self.auth_state == "verified"

    @property
    def state_title(self) -> str:
        return AUTH_STATES[self.auth_state][0]

    @property
    def state_tone(self) -> str:
        return AUTH_STATES[self.auth_state][1]

    @property
    def method_title(self) -> str:
        return AUTH_METHODS.get(self.auth_method or "", "—")
