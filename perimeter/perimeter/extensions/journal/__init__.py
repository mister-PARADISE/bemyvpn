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
PER_PAGE = 80

router = APIRouter()

# Метка строки лога по последнему слову действия: target.authorized → [ok]
LEVELS = {
    "created": ("[+]", "accent"),
    "authorized": ("[ok]", "success"),
    "setup": ("[ok]", "success"),
    "login": ("[>]", ""),
    "revoked": ("[x]", "danger"),
    "deleted": ("[x]", "danger"),
}


def level(action: str) -> tuple[str, str]:
    return LEVELS.get(action.rsplit(".", 1)[-1], ("[~]", ""))


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
    tabs = [("", "Всё", None)] + [(k, title, None) for k, title in ctx.entity_kinds()] + [("system", "Система", None)]
    return render(request, "journal/index.html", entries=entries, kind=kind, q=q, tabs=tabs, page=page, pages=pages,
                  total=total, query={"kind": kind, "q": q}, level=level)


def recent(db, ctx: Context) -> dict:
    return {"entries": db.scalars(select(AuditEntry).order_by(AuditEntry.id.desc()).limit(8)).all(), "level": level}


def for_entity(kind: str):
    def data(db, ctx: Context, **kwargs) -> dict | None:
        obj = kwargs.get(kind)
        if obj is None:
            return None
        entries = db.scalars(select(AuditEntry).where(AuditEntry.entity_type == kind, AuditEntry.entity_id == obj.id)
                             .order_by(AuditEntry.id.desc()).limit(10)).all()
        return {"entries": entries, "level": level}
    return data


def setup(ctx: Context) -> None:
    ctx.pages.add(Page("journal", "Журнал", "/journal", icon="history", section="Система", order=90,
                       description="Кто, что и когда сделал"))
    ctx.add_router(router)
    ctx.widgets.add(Widget("recent", "Журнал", "journal/_widget.html", data=recent, icon="history",
                           order=90, span=3, link="/journal"))
    ctx.add_slot("target.side", Slot("journal", "journal/_entity.html", data=for_entity("target"), order=90))
