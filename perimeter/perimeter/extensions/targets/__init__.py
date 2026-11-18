"""Цели: сайты и их границы.

Одна отметка — «владелец согласен». Без неё работают только пассивные модули:
они берут данные из открытых источников и к сайту не обращаются (так можно
показать будущему клиенту, что про него видно снаружи). С отметкой — все.
Клиенту ничего делать не нужно: отметку ставите вы.
"""

from __future__ import annotations

import math

from fastapi import APIRouter, HTTPException, Request
from sqlalchemy import func, or_, select
from sqlalchemy.engine import Connection

from ...core import fields as f
from ...core.context import Context, Page, SearchHit, Widget
from ...core.db import utcnow
from ...core.modules import Kind
from ...core.scope import ScopeError, display_host, normalize_host, parse_rules
from ...web import DB, actor, flash, form_data, get_ctx, redirect, render
from .models import Target

TITLE = "Цели"
DESCRIPTION = "Сайты, их границы и отметка о согласии владельца"
VERSION = "2.0"
PER_PAGE = 50

PASSIVE_HINT = "Без согласия работают только пассивные модули: открытые источники, к сайту запросов нет"

TARGET_FIELDS = (
    f.text("domain", "Домен сайта", required=True, max_length=253, width="half", placeholder="example.com",
           help="Поддомены входят в границы сами"),
    f.text("title", "Название", max_length=160, width="half", placeholder="Интернет-магазин"),
    f.boolean("consent", "Владелец согласен на проверку", help=PASSIVE_HINT),
    f.text("consent_note", "Откуда согласие", max_length=300, placeholder="попросил в Telegram 18.11",
           help="Необязательно. Пригодится, если когда-нибудь спросят", show_if="consent"),
    f.lines("include", "Дополнительно можно", max_length=253,
            placeholder="cdn.example.net\n203.0.113.10\n198.51.100.0/24",
            help="По одному в строке: другой домен, IP или подсеть владельца", width="half"),
    f.lines("exclude", "Не трогать", max_length=253, placeholder="pay.example.com",
            help="Исключения важнее остального", width="half"),
    f.textarea("notes", "Заметки", help="Видите только вы"),
)

MODE_TABS = (("", "Все"), ("yes", "С согласием"), ("no", "Только пассивные"))

router = APIRouter()


def _target_or_404(db, target_id: int) -> Target:
    target = db.get(Target, target_id)
    if target is None:
        raise HTTPException(404, "Цель не найдена")
    return target


def _validate(db, values: dict, errors: dict, current: Target | None = None) -> dict:
    """Нормализует домен и правила границ, дописывает ошибки в errors."""
    if "domain" in values and "domain" not in errors:
        try:
            values["domain"] = normalize_host(values["domain"], strip_www=True)
        except ScopeError as exc:
            errors["domain"] = str(exc)
        else:
            clash = db.scalar(select(Target).where(Target.domain == values["domain"]))
            if clash is not None and (current is None or clash.id != current.id):
                errors["domain"] = f"Этот домен уже есть: «{clash.title or clash.name}»"
    for key in ("include", "exclude"):
        if key in values and key not in errors:
            good, bad = parse_rules(values[key] or [])
            if bad:
                errors[key] = "Не разобрал: " + "; ".join(bad)
            values[key] = good
    for key in ("title", "notes", "consent_note"):
        values[key] = values.get(key) or ""
    return values


def _set_consent(target: Target, on: bool, note: str | None = None) -> bool:
    """Ставит или снимает отметку. True — если что-то поменялось."""
    if note is not None:
        target.consent_note = note
    if on == target.consent:
        return False
    target.consent = on
    target.consent_at = utcnow() if on else None
    return True


def _search_clause(q: str):
    like = f"%{q}%"
    try:
        host_like = f"%{normalize_host(q)}%"
    except ScopeError:
        host_like = like
    return or_(Target.domain.ilike(like), Target.domain.ilike(host_like), Target.title.ilike(like))


# --- список и создание ------------------------------------------------------

@router.get("/targets")
def targets_list(request: Request, db: DB, q: str = "", mode: str = "", page: int = 1):
    total_all = db.scalar(select(func.count()).select_from(Target)) or 0
    with_consent = db.scalar(select(func.count()).select_from(Target).where(Target.consent.is_(True))) or 0
    stmt = select(Target)
    if mode == "yes":
        stmt = stmt.where(Target.consent.is_(True))
    elif mode == "no":
        stmt = stmt.where(Target.consent.is_(False))
    if q.strip():
        stmt = stmt.where(_search_clause(q.strip()))
    total = db.scalar(select(func.count()).select_from(stmt.subquery())) or 0
    pages = max(1, math.ceil(total / PER_PAGE))
    page = min(max(page, 1), pages)
    rows = db.scalars(stmt.order_by(Target.updated_at.desc()).offset((page - 1) * PER_PAGE).limit(PER_PAGE)).all()
    counts = {"": total_all, "yes": with_consent, "no": total_all - with_consent}
    tabs = [(k, title, counts[k]) for k, title in MODE_TABS]
    return render(request, "targets/list.html", targets=rows, q=q, mode=mode, tabs=tabs, page=page,
                  pages=pages, total=total, everything=total_all, query={"q": q, "mode": mode})


@router.get("/targets/new")
def target_new(request: Request):
    return render(request, "targets/form.html", target=None, fields=TARGET_FIELDS,
                  values=f.initial(TARGET_FIELDS), errors={})


@router.post("/targets/new")
async def target_create(request: Request, db: DB):
    values, errors = f.parse_form(TARGET_FIELDS, await form_data(request))
    values = _validate(db, values, errors)
    if errors:
        return render(request, "targets/form.html", 422, target=None, fields=TARGET_FIELDS, values=values,
                      errors=errors)
    consent = values.pop("consent")
    target = Target(**values)
    _set_consent(target, consent)
    db.add(target)
    db.flush()
    mode = "с согласием владельца" if target.consent else "только пассивные проверки"
    get_ctx(request).audit(db, "target.created", f"Новая цель {target.name}: {mode}", entity=("target", target.id),
                           actor=actor(request))
    flash(request, "Цель добавлена")
    return redirect(f"/targets/{target.id}")


# --- карточка ----------------------------------------------------------------

@router.get("/targets/{target_id}")
def target_detail(request: Request, db: DB, target_id: int):
    target = _target_or_404(db, target_id)
    modules = [m for m in get_ctx(request).modules if {"domain", "subdomain"} & set(m.spec.consumes)]
    return render(request, "targets/detail.html", target=target, modules=modules, Kind=Kind, hint=PASSIVE_HINT)


@router.get("/targets/{target_id}/edit")
def target_edit(request: Request, db: DB, target_id: int):
    target = _target_or_404(db, target_id)
    values = f.initial(TARGET_FIELDS, target)
    values["domain"] = target.name
    return render(request, "targets/form.html", target=target, fields=TARGET_FIELDS, values=values, errors={})


@router.post("/targets/{target_id}/edit")
async def target_update(request: Request, db: DB, target_id: int):
    target = _target_or_404(db, target_id)
    values, errors = f.parse_form(TARGET_FIELDS, await form_data(request))
    values = _validate(db, values, errors, current=target)
    if errors:
        return render(request, "targets/form.html", 422, target=target, fields=TARGET_FIELDS, values=values,
                      errors=errors)
    changed = [fld.label for fld in TARGET_FIELDS if getattr(target, fld.key) != values.get(fld.key)]
    consent = values.pop("consent")
    for key, value in values.items():
        setattr(target, key, value)
    _set_consent(target, consent)
    if changed:
        get_ctx(request).audit(db, "target.updated", f"Изменена цель {target.name}: {', '.join(changed)}",
                               entity=("target", target.id), actor=actor(request))
    flash(request, "Сохранено" if changed else "Ничего не изменилось", "success" if changed else "info")
    return redirect(f"/targets/{target.id}")


@router.post("/targets/{target_id}/consent")
async def target_consent(request: Request, db: DB, target_id: int):
    target = _target_or_404(db, target_id)
    note = str((await form_data(request)).get("consent_note", "")).strip()[:300]
    if _set_consent(target, True, note):
        source = f" ({note})" if note else ""
        get_ctx(request).audit(db, "target.consent", f"Согласие владельца {target.name}{source}",
                               entity=("target", target.id), actor=actor(request))
        flash(request, "Отмечено. Для этой цели открыты все модули")
    return redirect(f"/targets/{target.id}")


@router.post("/targets/{target_id}/consent/remove")
def target_consent_remove(request: Request, db: DB, target_id: int):
    target = _target_or_404(db, target_id)
    if _set_consent(target, False):
        get_ctx(request).audit(db, "target.consent_removed", f"Снята отметка о согласии {target.name}",
                               entity=("target", target.id), actor=actor(request))
        flash(request, "Отметка снята. Остались только пассивные модули", "info")
    return redirect(f"/targets/{target.id}")


@router.post("/targets/{target_id}/delete")
def target_delete(request: Request, db: DB, target_id: int):
    target = _target_or_404(db, target_id)
    name = target.name
    db.delete(target)
    get_ctx(request).audit(db, "target.deleted", f"Удалена цель {name}", actor=actor(request))
    flash(request, f"Цель {name} удалена")
    return redirect("/targets")


@router.get("/targets/{target_id}/scope")
def target_scope_check(request: Request, db: DB, target_id: int, host: str = ""):
    target = _target_or_404(db, target_id)
    allowed, reason = target.scope.explain(host)
    try:
        normalized = display_host(normalize_host(host))
    except ScopeError:
        normalized = host
    return {"allowed": allowed, "reason": reason, "host": normalized}


# --- виджет, поиск ----------------------------------------------------------------

def targets_widget(db, ctx: Context) -> dict:
    total = db.scalar(select(func.count()).select_from(Target)) or 0
    with_consent = db.scalar(select(func.count()).select_from(Target).where(Target.consent.is_(True))) or 0
    latest = db.scalars(select(Target).order_by(Target.updated_at.desc()).limit(5)).all()
    return {"total": total, "with_consent": with_consent, "latest": latest}


def search_targets(db, q: str) -> list[SearchHit]:
    rows = db.scalars(select(Target).where(_search_clause(q)).limit(8)).all()
    return [SearchHit(t.name, f"/targets/{t.id}", "Цели", t.title or t.mode_title, "globe") for t in rows]


# --- миграции ------------------------------------------------------------------

def _v2_consent(conn: Connection) -> None:
    """Версия 1 хранила «разрешение» с кодами и сроками. Теперь — одна отметка о согласии."""
    columns = {row[1] for row in conn.exec_driver_sql("PRAGMA table_info(target)")}
    if "consent" not in columns:
        conn.exec_driver_sql("ALTER TABLE target ADD COLUMN consent BOOLEAN NOT NULL DEFAULT 0")
        conn.exec_driver_sql("ALTER TABLE target ADD COLUMN consent_note TEXT NOT NULL DEFAULT ''")
        conn.exec_driver_sql("ALTER TABLE target ADD COLUMN consent_at DATETIME")
    if "auth_status" in columns:
        conn.exec_driver_sql(
            "UPDATE target SET consent = 1, consent_note = auth_note, consent_at = auth_verified_at "
            "WHERE auth_status = 'verified'")
        conn.exec_driver_sql("DROP INDEX IF EXISTS ix_target_auth_status")
        for col in ("auth_status", "auth_method", "auth_token", "auth_note", "auth_verified_at", "auth_valid_until"):
            if col in columns:
                conn.exec_driver_sql(f"ALTER TABLE target DROP COLUMN {col}")
    conn.exec_driver_sql("CREATE INDEX IF NOT EXISTS ix_target_consent ON target (consent)")


def setup(ctx: Context) -> None:
    ctx.pages.add(Page("targets", "Цели", "/targets", icon="globe", order=20, description="Сайты и их границы"))
    ctx.add_router(router)
    ctx.widgets.add(Widget("targets", "Цели", "targets/_widget.html", data=targets_widget, icon="globe",
                           order=10, link="/targets"))
    ctx.search(search_targets)
    ctx.entity("target", lambda id: f"/targets/{id}", "Цели")
    ctx.db.migration("targets", 2, _v2_consent)
