"""Журнал: каждое действие в панели — кто, что и когда. Вставляется и в карточки объектов."""

from __future__ import annotations

import math

from fastapi import APIRouter, Request
from sqlalchemy import func, or_, select

from ...core.context import Context, Page, Slot, Widget
from ...core.models import AuditEntry
from ...web import DB, get_ctx, render

TITLE = "Журнал"
DESCRIPTION = "История действий в панели"
VERSION = "1.0"
PER_PAGE = 60

router = APIRouter()

GOOD = ("authorized", "paid", "setup")
BAD = ("revoked", "deleted", "cancelled")
NEW = ("created",)


def tone(action: str) -> str:
    tail = action.rsplit(".", 1)[-1]
    if tail in GOOD:
        return "success"
    if tail in BAD:
        return "danger"
    if tail in NEW:
        return "accent"
    return ""


@router.get("/journal")
def journal(request: Request, db: DB, kind: str = "", q: str = "", page: int = 1):
    ctx = get_ctx(request)
    stmt = select(AuditEntry)
    if kind == "system":
        stmt = stmt.where(AuditEntry.entity_type.is_(None))
    elif kind:
        stmt = stmt.where(AuditEntry.entity_type == kind)
    if q.strip():
        like = f"%{q.strip()}%"
        stmt = stmt.where(or_(AuditEntry.summary.ilike(like), AuditEntry.actor.ilike(like)))
    total = db.scalar(select(func.count()).select_from(stmt.subquery())) or 0
    pages = max(1, math.ceil(total / PER_PAGE))
    page = min(max(page, 1), pages)
    entries = db.scalars(stmt.order_by(AuditEntry.id.desc()).offset((page - 1) * PER_PAGE).limit(PER_PAGE)).all()
    tabs = [("", "Всё", None)] + [(k, title, None) for k, title in ctx.entity_kinds()] + [("system", "Вход и система", None)]
    return render(request, "journal/index.html", entries=entries, kind=kind, q=q, tabs=tabs, page=page, pages=pages,
                  total=total, query={"kind": kind, "q": q}, tone=tone)


def recent(db, ctx: Context) -> dict:
    return {"entries": db.scalars(select(AuditEntry).order_by(AuditEntry.id.desc()).limit(7)).all(), "tone": tone}


def for_entity(kind: str):
    def data(db, ctx: Context, **kwargs) -> dict | None:
        obj = kwargs.get(kind)
        if obj is None:
            return None
        entries = db.scalars(select(AuditEntry).where(AuditEntry.entity_type == kind, AuditEntry.entity_id == obj.id)
                             .order_by(AuditEntry.id.desc()).limit(8)).all()
        return {"entries": entries, "tone": tone, "more": f"/journal?kind={kind}&q="}
    return data


def setup(ctx: Context) -> None:
    ctx.pages.add(Page("journal", "Журнал", "/journal", icon="history", section="Система", order=90,
                       description="Кто, что и когда сделал"))
    ctx.add_router(router)
    ctx.widgets.add(Widget("recent", "Последние действия", "journal/_widget.html", data=recent, icon="history",
                           order=90, link="/journal"))
    ctx.add_slot("client.side", Slot("journal", "journal/_entity.html", data=for_entity("client"), order=90))
    ctx.add_slot("target.side", Slot("journal", "journal/_entity.html", data=for_entity("target"), order=90))
