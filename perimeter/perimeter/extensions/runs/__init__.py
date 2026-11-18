"""Запуски: движок, консоль с живым выводом, находки по целям.

Модули запускаются из карточки цели: по одному или «всё сразу» цепочкой.
Вывод идёт в консоль на той же странице, находки складываются в «Найдено».
Как показывать находки своего вида, расширение говорит через
ctx.services["asset_columns"][вид] = [(заголовок, функция(asset) → текст или (текст, оттенок))].
"""

from __future__ import annotations

import math
from datetime import timezone
from typing import Any
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError

from fastapi import APIRouter, HTTPException, Request
from sqlalchemy import func, select

from ...core import fields as f
from ...core.context import Context, Page, Slot, Stat
from ...core.db import utcnow
from ...core.modules import Kind
from ...core.settings import SettingsSection
from ...web import DB, actor, flash, form_data, get_ctx, redirect, render
from ..targets.models import Target
from .engine import Engine
from .models import ACTIVE_STATES, RUN_STATES, Asset, Run, RunLine

TITLE = "Запуски"
DESCRIPTION = "Движок модулей, консоль, находки"
VERSION = "1.0"
PER_PAGE = 50

router = APIRouter()


def engine_of(request: Request) -> Engine:
    return get_ctx(request).services["engine"]


def applicable(ctx: Context) -> list:
    """Модули, которые можно запустить по цели: принимают домен, поддомен или IP."""
    return [m for m in ctx.modules if {"domain", "subdomain", "ip"} & set(m.spec.consumes)]


def _target_or_404(db, target_id: int) -> Target:
    target = db.get(Target, target_id)
    if target is None:
        raise HTTPException(404, "Цель не найдена")
    return target


def _run_or_404(db, run_id: int) -> Run:
    run = db.get(Run, run_id)
    if run is None:
        raise HTTPException(404, "Запуск не найден")
    return run


# --- запуск ------------------------------------------------------------------

@router.post("/targets/{target_id}/run")
async def start(request: Request, db: DB, target_id: int):
    target = _target_or_404(db, target_id)
    ctx = get_ctx(request)
    form = await form_data(request)
    module_id = str(form.get("module", ""))
    if module_id == "__all__":
        # сначала пассивные (они находят адреса), потом активные — по порядку модулей
        chosen = sorted(applicable(ctx), key=lambda m: (m.spec.kind is Kind.ACTIVE, m.spec.order))
        plans = [(m, {fld.key: fld.default for fld in m.spec.params}) for m in chosen]
    else:
        module = ctx.modules.get(module_id)
        if module is None:
            raise HTTPException(404, "Модуль не найден")
        if form.get("with_params"):
            raw = {fld.key: form.get(f"p_{fld.key}") for fld in module.spec.params}
            params, errors = f.parse_form(module.spec.params, raw)
        else:  # быстрый запуск ▶ — параметры по умолчанию (пустой чекбокс в форме значил бы «выключено»)
            params, errors = {fld.key: fld.default for fld in module.spec.params}, {}
        if errors:
            labels = {fld.key: fld.label for fld in module.spec.params}
            flash(request, "Не запущено: " + "; ".join(f"{labels[k]} — {v}" for k, v in errors.items()), "error")
            return redirect(f"/targets/{target.id}")
        plans = [(module, params)]
    if not plans:
        flash(request, "Нет модулей, которые можно запустить по этой цели", "info")
        return redirect(f"/targets/{target.id}")
    runs = [Run(target_id=target.id, module_id=m.spec.id, params=p, actor=actor(request)) for m, p in plans]
    db.add_all(runs)
    db.commit()  # движок работает в своём потоке и должен увидеть запуски сразу
    engine_of(request).submit([r.id for r in runs])
    return redirect(f"/targets/{target.id}?run={runs[0].id}#console")


@router.post("/runs/{run_id}/cancel")
async def cancel(request: Request, db: DB, run_id: int):
    run = _run_or_404(db, run_id)
    back = str((await form_data(request)).get("back") or f"/runs/{run.id}")
    if not back.startswith("/"):
        back = f"/runs/{run.id}"
    if run.status == "queued":
        run.status, run.finished_at = "cancelled", utcnow()
        db.add(RunLine(run_id=run.id, n=1, level="warn", text="[stop] отменён до начала"))
        flash(request, "Запуск отменён", "info")
    elif run.status == "running":
        engine_of(request).cancel(run.id)
        flash(request, "Останавливаю…", "info")
    return redirect(back)


@router.post("/runs/{run_id}/again")
def again(request: Request, db: DB, run_id: int):
    old = _run_or_404(db, run_id)
    run = Run(target_id=old.target_id, module_id=old.module_id, params=dict(old.params or {}), actor=actor(request))
    db.add(run)
    db.commit()
    engine_of(request).submit([run.id])
    return redirect(f"/targets/{old.target_id}?run={run.id}#console")


# --- живой вывод ---------------------------------------------------------------

def _clock(ctx: Context, when) -> str:
    try:
        tz = ZoneInfo(ctx.settings.get("general.timezone"))
    except (ZoneInfoNotFoundError, ValueError):
        tz = timezone.utc
    return when.replace(tzinfo=timezone.utc).astimezone(tz).strftime("%H:%M:%S")


def run_state(ctx: Context, db, run: Run, after: int = 0) -> dict[str, Any]:
    lines = db.scalars(select(RunLine).where(RunLine.run_id == run.id, RunLine.n > after)
                       .order_by(RunLine.n).limit(1000)).all()
    return {
        "id": run.id,
        "status": run.status,
        "title": run.state_title,
        "tone": run.state_tone,
        "active": run.active,
        "progress": run.progress,
        "seconds": round(run.seconds or 0, 1),
        "summary": run.summary,
        "error": run.error,
        "found_new": run.found_new,
        "lines": [{"n": ln.n, "level": ln.level, "text": ln.text, "at": _clock(ctx, ln.at)}
                  for ln in lines],
    }


@router.get("/runs/{run_id}/log")
def run_log(request: Request, db: DB, run_id: int, after: int = 0):
    return run_state(get_ctx(request), db, _run_or_404(db, run_id), after)


# --- страницы ------------------------------------------------------------------

TABS = (("", "Все"), ("active", "Идут"), ("done", "Готово"), ("failed", "Ошибки"))


@router.get("/runs")
def runs_list(request: Request, db: DB, state: str = "", page: int = 1):
    ctx = get_ctx(request)
    stmt = select(Run)
    if state == "active":
        stmt = stmt.where(Run.status.in_(ACTIVE_STATES))
    elif state in ("done", "failed"):
        stmt = stmt.where(Run.status == state)
    total = db.scalar(select(func.count()).select_from(stmt.subquery())) or 0
    pages = max(1, math.ceil(total / PER_PAGE))
    page = min(max(page, 1), pages)
    rows = db.scalars(stmt.order_by(Run.id.desc()).offset((page - 1) * PER_PAGE).limit(PER_PAGE)).all()
    targets = {t.id: t for t in db.scalars(select(Target).where(Target.id.in_({r.target_id for r in rows})))}
    counts = dict(db.execute(select(Run.status, func.count()).group_by(Run.status)).all())
    tab_counts = {"": sum(counts.values()), "active": sum(counts.get(s, 0) for s in ACTIVE_STATES),
                  "done": counts.get("done", 0), "failed": counts.get("failed", 0)}
    return render(request, "runs/list.html", runs=rows, targets=targets, state=state, page=page, pages=pages,
                  total=total, query={"state": state}, tabs=[(k, t, tab_counts[k]) for k, t in TABS],
                  modules=ctx.modules)


@router.get("/runs/{run_id}")
def run_page(request: Request, db: DB, run_id: int):
    run = _run_or_404(db, run_id)
    target = db.get(Target, run.target_id)
    module = get_ctx(request).modules.get(run.module_id)
    return render(request, "runs/detail.html", run=run, target=target, module=module,
                  state=run_state(get_ctx(request), db, run))


# --- находки -------------------------------------------------------------------

@router.post("/targets/{target_id}/assets/clear")
async def clear_assets(request: Request, db: DB, target_id: int):
    target = _target_or_404(db, target_id)
    kind = str((await form_data(request)).get("kind", ""))
    stmt = select(Asset).where(Asset.target_id == target.id)
    if kind:
        stmt = stmt.where(Asset.kind == kind)
    n = 0
    for asset in db.scalars(stmt):
        db.delete(asset)
        n += 1
    get_ctx(request).audit(db, "target.assets_cleared", f"{target.name}: удалено находок {n}",
                           entity=("target", target.id), actor=actor(request))
    flash(request, f"Удалено находок: {n}")
    return redirect(f"/targets/{target.id}#found")


# --- вставки в карточку цели ---------------------------------------------------------

def modules_slot(db, ctx: Context, target=None, run=None, **_):
    if target is None:
        return None
    mods = applicable(ctx)
    last = {}
    for r in db.scalars(select(Run).where(Run.target_id == target.id).order_by(Run.id.desc()).limit(200)):
        last.setdefault(r.module_id, r)
    console = None
    if run and str(run).isdigit():
        console = db.get(Run, int(run))
        if console is not None and console.target_id != target.id:
            console = None
    if console is None:
        console = db.scalar(select(Run).where(Run.target_id == target.id, Run.status.in_(ACTIVE_STATES))
                            .order_by(Run.id))
    state = run_state(ctx, db, console) if console is not None else None
    return {"mods": mods, "last": last, "console": console, "console_state": state, "Kind": Kind,
            "nav_count": len(mods), "nav_tone": "warn" if state and state.get("active") else ""}


def found_slot(db, ctx: Context, target=None, **_):
    if target is None:
        return None
    rows = db.scalars(select(Asset).where(Asset.target_id == target.id).order_by(Asset.kind, Asset.id)).all()
    groups: dict[str, list[Asset]] = {}
    for a in rows:
        groups.setdefault(a.kind, []).append(a)
    order = {t.id: t.order for t in ctx.asset_types}
    kinds = sorted(groups, key=lambda k: (k != "ip", order.get(k, 999), k))  # адреса — самое нужное
    columns = ctx.services.get("asset_columns", {})
    return {"groups": groups, "kinds": kinds, "columns": columns, "types": ctx.asset_types, "nav_count": len(rows)}


def default_columns(asset: Asset) -> list[tuple[str, str]]:
    data = {k: v for k, v in (asset.data or {}).items() if v not in (None, "", [], {})}
    return [("значение", asset.value), ("данные", ", ".join(f"{k}={v}" for k, v in list(data.items())[:6]) or "—")]


def cell(columns: dict, asset: Asset) -> list[tuple[str, str, str]]:
    """[(заголовок, текст, оттенок)] для строки находки."""
    spec = columns.get(asset.kind)
    if spec is None:
        return [(t, v, "") for t, v in default_columns(asset)]
    out = []
    for title, fn in spec:
        try:
            value = fn(asset)
        except Exception:  # noqa: BLE001
            value = "?"
        text, tone = value if isinstance(value, tuple) else (value, "")
        out.append((title, "—" if text in (None, "") else str(text), tone))
    return out


# --- миграции, счётчики, настройки ---------------------------------------------------------

def _active_badge(db) -> int:
    return db.scalar(select(func.count()).select_from(Run).where(Run.status.in_(ACTIVE_STATES))) or 0


def runs_stat(db, ctx: Context):
    active = _active_badge(db)
    last = db.scalar(select(Run).where(Run.finished_at.is_not(None)).order_by(Run.finished_at.desc()))
    if active:
        return f"идёт {active}", "ok"
    if last is None:
        return "ещё не было"
    return f"последний {_ago(last.finished_at)}"


def _ago(when) -> str:
    seconds = int((utcnow() - when).total_seconds())
    if seconds < 60:
        return "только что"
    if seconds < 3600:
        return f"{seconds // 60} мин назад"
    if seconds < 86400:
        return f"{seconds // 3600} ч назад"
    return f"{seconds // 86400} дн назад"


def on_ready(ctx: Context) -> None:
    """Панель перезапустили посреди скана — честно помечаем такие запуски прерванными."""
    with ctx.db.session() as s:
        for run in s.scalars(select(Run).where(Run.status.in_(ACTIVE_STATES))):
            n = (s.scalar(select(func.max(RunLine.n)).where(RunLine.run_id == run.id)) or 0) + 1
            s.add(RunLine(run_id=run.id, n=n, level="warn", text="[stop] прерван: панель перезапустили"))
            run.status, run.finished_at = "interrupted", utcnow()


def setup(ctx: Context) -> None:
    engine = Engine(ctx)
    ctx.services["engine"] = engine
    ctx.services.setdefault("asset_columns", {})
    ctx.template_globals["asset_cells"] = cell
    ctx.template_globals["RUN_STATES"] = RUN_STATES
    ctx.pages.add(Page("runs", "Запуски", "/runs", icon="activity", order=30, badge=_active_badge,
                       badge_tone="accent", description="Что запускалось и что идёт сейчас"))
    ctx.add_router(router)
    ctx.add_slot("target.main", Slot("modules", "runs/_modules.html", data=modules_slot, order=20, nav="модули"))
    ctx.add_slot("target.main", Slot("found", "runs/_found.html", data=found_slot, order=30, nav="найдено"))
    ctx.stats.add(Stat("runs", "запуски", runs_stat, order=30, link="/runs"))
    ctx.settings.section(SettingsSection(
        id="runs", title="Запуски", icon="activity", group="Модули", order=10,
        description="Сколько модулей может работать одновременно.",
        fields=(f.integer("parallel", "Одновременно запусков", default=3, min=1, max=10, restart=True,
                          help="Остальные ждут в очереди"),),
    ))
    ctx.hook("app.ready", on_ready)
    ctx.hook("app.stop", lambda _ctx: engine.stop())
