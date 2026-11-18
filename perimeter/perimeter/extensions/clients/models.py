from __future__ import annotations

from datetime import date
from decimal import Decimal

from sqlalchemy import Date, ForeignKey, Integer, Numeric, String, Text
from sqlalchemy.orm import Mapped, mapped_column, relationship

from ...core.db import Base, TimestampMixin

CLIENT_STATUSES = {
    "lead": ("Обсуждаем", "info"),
    "active": ("Работаем", "success"),
    "paused": ("Пауза", "warning"),
    "archived": ("Архив", "neutral"),
}

INVOICE_STATUSES = {
    "draft": ("Черновик", "neutral"),
    "issued": ("Ждёт оплаты", "warning"),
    "paid": ("Оплачен", "success"),
    "cancelled": ("Отменён", "neutral"),
}


class Client(TimestampMixin, Base):
    __tablename__ = "client"

    id: Mapped[int] = mapped_column(Integer, primary_key=True)
    name: Mapped[str] = mapped_column(String(160), nullable=False)
    company: Mapped[str] = mapped_column(String(160), default="", nullable=False)
    email: Mapped[str] = mapped_column(String(200), default="", nullable=False)
    telegram: Mapped[str] = mapped_column(String(100), default="", nullable=False)
    phone: Mapped[str] = mapped_column(String(50), default="", nullable=False)
    status: Mapped[str] = mapped_column(String(16), default="lead", nullable=False, index=True)
    paid_until: Mapped[date | None] = mapped_column(Date)
    notes: Mapped[str] = mapped_column(Text, default="", nullable=False)

    invoices: Mapped[list["Invoice"]] = relationship(
        back_populates="client", cascade="all, delete-orphan", order_by="Invoice.id.desc()"
    )

    @property
    def status_title(self) -> str:
        return CLIENT_STATUSES.get(self.status, (self.status, "neutral"))[0]

    @property
    def status_tone(self) -> str:
        return CLIENT_STATUSES.get(self.status, (self.status, "neutral"))[1]

    @property
    def display(self) -> str:
        return f"{self.name} · {self.company}" if self.company else self.name


class Invoice(TimestampMixin, Base):
    __tablename__ = "invoice"

    id: Mapped[int] = mapped_column(Integer, primary_key=True)
    client_id: Mapped[int] = mapped_column(ForeignKey("client.id", ondelete="CASCADE"), nullable=False, index=True)
    number: Mapped[str] = mapped_column(String(40), nullable=False, unique=True)
    title: Mapped[str] = mapped_column(String(300), default="", nullable=False)
    amount: Mapped[Decimal] = mapped_column(Numeric(12, 2), nullable=False)
    currency: Mapped[str] = mapped_column(String(8), default="USD", nullable=False)
    status: Mapped[str] = mapped_column(String(16), default="issued", nullable=False, index=True)
    issued_on: Mapped[date] = mapped_column(Date, default=date.today, nullable=False)
    due_on: Mapped[date | None] = mapped_column(Date)
    paid_on: Mapped[date | None] = mapped_column(Date)
    # Если указано: после оплаты «Оплачено до» у клиента продлевается до этой даты.
    covers_until: Mapped[date | None] = mapped_column(Date)

    client: Mapped[Client] = relationship(back_populates="invoices")

    @property
    def status_title(self) -> str:
        return INVOICE_STATUSES.get(self.status, (self.status, "neutral"))[0]

    @property
    def status_tone(self) -> str:
        if self.is_overdue:
            return "danger"
        return INVOICE_STATUSES.get(self.status, (self.status, "neutral"))[1]

    @property
    def is_overdue(self) -> bool:
        return self.status == "issued" and self.due_on is not None and self.due_on < date.today()
