"""Цели: сайты, их границы и разрешение владельца на проверку.

Разрешение — главный предохранитель. Активные модули (те, что шлют запросы
сайту) запустятся только для цели с действующим разрешением и только в её
границах. Пассивные модули (публичные источники) разрешения не требуют.
"""

from __future__ import annotations

import math
from datetime import date, timedelta

from fastapi import APIRouter, HTTPException, Request
from sqlalchemy import func, or_, select

from ...core import fields as f
from ...core.context import Context, Page, SearchHit, Widget
from ...core.db import utcnow
from ...core.modules import Kind
from ...core.scope import ScopeError, display_host, normalize_host, parse_rules
from ...web import DB, actor, flash, form_data, get_ctx, redirect, render
from .models import AUTH_METHODS, AUTH_STATES, Target, new_auth_token

TITLE = "Цели"
DESCRIPTION = "Сайты, границы проверки и разрешения владельцев"
VERSION = "1.0"
PER_PAGE = 50
VERIFY_FILE = "/.well-known/perimeter-verify.txt"

TARGET_FIELDS = (
    f.text("domain", "Домен сайта", required=True, max_length=253, width="half", placeholder="example.com",
           help="Поддомены входят в границы сами"),
    f.text("title", "Название", max_length=160, width="half", placeholder="Интернет-магазин"),
    f.lines("include", "Дополнительно разрешено", max_length=253,
            placeholder="cdn.example.net\n203.0.113.10\n198.51.100.0/24",
            help="По одному в строке: другой домен, IP или подсеть владельца", width="half"),
    f.lines("exclude", "Не трогать", max_length=253, placeholder="pay.example.com",
            help="Исключения важнее разрешений", width="half"),
    f.textarea("notes", "Заметки", help="Видите только вы"),
)

AUTH_FIELDS = (
    f.choice("method", "Чем подтверждено", AUTH_METHODS, width="half"),
    f.date("valid_until", "Действует до", width="half", help="Пусто — бессрочно"),
    f.textarea("note", "Подтверждение", required=True, max_length=2000,
               placeholder="Договор №12 от 18.11.2026, скан в папке проекта",
               help="Номер договора, дата письма, где лежит документ. Это ваша защита, если возникнут вопросы"),
)

STATE_TABS = (("", "Все"), ("verified", "Разрешены"), ("pending", "Ждут"), ("expired", "Истекли"),
              ("revoked", "Отозваны"))

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
    for key in ("title", "notes"):
        values[key] = values.get(key) or ""
    return values


def _search_clause(q: str):
    like = f"%{q}%"
    try:
        host_like = f"%{normalize_host(q)}%"
    except ScopeError:
        host_like = like
    return or_(Target.domain.ilike(like), Target.domain.ilike(host_like), Target.title.ilike(like))


# --- список и создание ------------------------------------------------------

@router.get("/targets")
def targets_list(request: Request, db: DB, q: str = "", state: str = "", page: int = 1):
    every = db.scalars(select(Target)).all()
    counts = {k: 0 for k in AUTH_STATES}
    for t in every:
        counts[t.auth_state] += 1
    stmt = select(Target)
    today = date.today()
    if state == "verified":
        stmt = stmt.where(Target.auth_status == "verified",
                          or_(Target.auth_valid_until.is_(None), Target.auth_valid_until >= today))
    elif state == "expired":
        stmt = stmt.where(Target.auth_status == "verified", Target.auth_valid_until < today)
    elif state in ("pending", "revoked"):
        stmt = stmt.where(Target.auth_status == state)
    if q.strip():
        stmt = stmt.where(_search_clause(q.strip()))
    total = db.scalar(select(func.count()).select_from(stmt.subquery())) or 0
    pages = max(1, math.ceil(total / PER_PAGE))
    page = min(max(page, 1), pages)
    rows = db.scalars(stmt.order_by(Target.updated_at.desc()).offset((page - 1) * PER_PAGE).limit(PER_PAGE)).all()
    tabs = [(k, title, len(every) if not k else counts[k]) for k, title in STATE_TABS]
    return render(request, "targets/list.html", targets=rows, q=q, state=state, tabs=tabs, page=page,
                  pages=pages, total=total, everything=len(every), query={"q": q, "state": state})


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
    target = Target(**values)
    db.add(target)
    db.flush()
    get_ctx(request).audit(db, "target.created", f"Новая цель {target.name}", entity=("target", target.id),
                           actor=actor(request))
    flash(request, "Цель добавлена. Осталось получить разрешение владельца")
    return redirect(f"/targets/{target.id}")


# --- карточка ----------------------------------------------------------------

@router.get("/targets/{target_id}")
def target_detail(request: Request, db: DB, target_id: int):
    target = _target_or_404(db, target_id)
    ctx = get_ctx(request)
    modules = [m for m in ctx.modules if {"domain", "subdomain"} & set(m.spec.consumes)]
    return render(request, "targets/detail.html", target=target, auth_fields=AUTH_FIELDS,
                  auth_values=f.initial(AUTH_FIELDS), modules=modules, Kind=Kind, verify_file=VERIFY_FILE)


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
    domain_changed = values["domain"] != target.domain
    for key, value in values.items():
        setattr(target, key, value)
    note = ""
    if domain_changed and target.auth_status == "verified":
        # Разрешение выдавалось на другой домен — оно больше не действует.
        target.auth_status = "pending"
        target.auth_token = new_auth_token()
        note = ". Домен изменился — нужно новое разрешение"
    if changed:
        get_ctx(request).audit(db, "target.updated", f"Изменена цель {target.name}: {', '.join(changed)}{note}",
                               entity=("target", target.id), actor=actor(request))
    flash(request, ("Сохранено" + note) if changed else "Ничего не изменилось",
          "warning" if note else ("success" if changed else "info"))
    return redirect(f"/targets/{target.id}")


@router.post("/targets/{target_id}/authorize")
async def target_authorize(request: Request, db: DB, target_id: int):
    target = _target_or_404(db, target_id)
    values, errors = f.parse_form(AUTH_FIELDS, await form_data(request))
    if not errors and values.get("valid_until") and values["valid_until"] < date.today():
        errors["valid_until"] = "Дата уже прошла"
    if errors:
        flash(request, "Разрешение не сохранено: " + "; ".join(errors.values()), "error")
        return redirect(f"/targets/{target.id}")
    target.auth_status = "verified"
    target.auth_method = values["method"]
    target.auth_note = values["note"]
    target.auth_valid_until = values.get("valid_until")
    target.auth_verified_at = utcnow()
    until = f" до {target.auth_valid_until:%d.%m.%Y}" if target.auth_valid_until else " бессрочно"
    get_ctx(request).audit(
        db, "target.authorized", f"Разрешение на {target.name}{until}: {target.method_title}",
        entity=("target", target.id), data={"method": target.auth_method, "note": target.auth_note},
        actor=actor(request))
    flash(request, "Разрешение сохранено. Активные проверки для этой цели открыты")
    return redirect(f"/targets/{target.id}")


@router.post("/targets/{target_id}/revoke")
def target_revoke(request: Request, db: DB, target_id: int):
    target = _target_or_404(db, target_id)
    if target.auth_status != "verified":
        return redirect(f"/targets/{target.id}")
    target.auth_status = "revoked"
    get_ctx(request).audit(db, "target.revoked", f"Разрешение на {target.name} отозвано",
                           entity=("target", target.id), actor=actor(request))
    flash(request, "Разрешение отозвано. Активные проверки закрыты", "warning")
    return redirect(f"/targets/{target.id}")


@router.post("/targets/{target_id}/token")
def target_new_token(request: Request, db: DB, target_id: int):
    target = _target_or_404(db, target_id)
    if target.is_authorized:
        flash(request, "Разрешение уже есть — код менять незачем", "info")
        return redirect(f"/targets/{target.id}")
    target.auth_token = new_auth_token()
    get_ctx(request).audit(db, "target.token", f"Новый код подтверждения для {target.name}",
                           entity=("target", target.id), actor=actor(request))
    flash(request, "Новый код подтверждения создан. Старый больше не подходит")
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


# --- виджет, поиск, счётчик -----------------------------------------------------

def targets_widget(db, ctx: Context) -> dict:
    remind = ctx.settings.get("general.remind_days")
    horizon = date.today() + timedelta(days=remind)
    targets = db.scalars(select(Target).order_by(Target.domain)).all()
    counts = {k: 0 for k in AUTH_STATES}
    for t in targets:
        counts[t.auth_state] += 1
    attention = [t for t in targets if t.auth_state in ("expired", "pending")]
    attention += [t for t in targets if t.auth_state == "verified" and t.auth_valid_until
                  and t.auth_valid_until <= horizon]
    return {"counts": counts, "total": len(targets), "attention": attention[:5]}


def search_targets(db, q: str) -> list[SearchHit]:
    rows = db.scalars(select(Target).where(_search_clause(q)).limit(8)).all()
    return [SearchHit(t.name, f"/targets/{t.id}", "Цели", t.title or t.state_title, "globe") for t in rows]


def _attention_badge(db) -> int:
    today = date.today()
    pending = db.scalar(select(func.count()).select_from(Target).where(Target.auth_status == "pending")) or 0
    expired = db.scalar(select(func.count()).select_from(Target).where(
        Target.auth_status == "verified", Target.auth_valid_until < today)) or 0
    return pending + expired


def setup(ctx: Context) -> None:
    ctx.pages.add(Page("targets", "Цели", "/targets", icon="globe", order=20, badge=_attention_badge,
                       badge_tone="warning", description="Сайты, границы и разрешения"))
    ctx.add_router(router)
    ctx.widgets.add(Widget("targets", "Цели", "targets/_widget.html", data=targets_widget, icon="globe",
                           order=10, link="/targets"))
    ctx.search(search_targets)
    ctx.entity("target", lambda id: f"/targets/{id}", "Цели")
