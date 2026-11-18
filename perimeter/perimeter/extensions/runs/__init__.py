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
from sqlalchemy import func, select, update

from ...core import fields as f
from ...core.context import Context, Page, SearchHit, Slot, Stat
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


def _rank(module) -> tuple:
    return (module.spec.kind is Kind.ACTIVE, module.spec.order, module.spec.id)


def plan(ctx: Context, db, target_id: int, wanted: list[str]) -> list:
    """Что запускать и в каком порядке: выбранные модули и то, на чём они построены (ModuleSpec.needs).

    Зависимость встаёт в цепочку первой, если по цели она ещё не отрабатывала, её находок нет
    (их очистили) или она старше того, на чём построена сама, — как в make. Свежую не повторяем.
    """
    mods = {m.spec.id: m for m in ctx.modules}
    last = dict(db.execute(select(Run.module_id, func.max(Run.finished_at))
                           .where(Run.target_id == target_id, Run.status == "done").group_by(Run.module_id)).all())
    kinds = set(db.scalars(select(Asset.kind).where(Asset.target_id == target_id).distinct()))
    memo: dict[str, bool] = {}

    def fresh(mid: str, path: tuple[str, ...] = ()) -> bool:
        if mid not in memo:
            m = mods.get(mid)
            memo[mid] = bool(
                m is not None and mid in last and mid not in path
                and (not m.spec.produces or kinds & set(m.spec.produces))
                and all(dep in mods and fresh(dep, (*path, mid)) and last[dep] <= last[mid] for dep in m.spec.needs))
        return memo[mid]

    order: list[str] = []
    chosen = set(wanted)

    def visit(mid: str, path: tuple[str, ...] = ()) -> None:
        if mid in order or mid in path or mid not in mods:
            return
        for dep in mods[mid].spec.needs:
            if dep in chosen or not fresh(dep):
                visit(dep, (*path, mid))
        order.append(mid)

    for mid in sorted((i for i in wanted if i in mods), key=lambda i: _rank(mods[i])):
        visit(mid)
    return [mods[i] for i in order]


def _defaults(module) -> dict[str, Any]:
    return {fld.key: fld.default for fld in module.spec.params}


def _queue(request: Request, db, target_id: int, items: list[tuple[str, dict[str, Any]]]) -> list[Run]:
    """Поставить запуски в очередь одной цепочкой и отдать движку."""
    runs = [Run(target_id=target_id, module_id=mid, params=params, actor=actor(request)) for mid, params in items]
    db.add_all(runs)
    db.flush()
    if len(runs) > 1:
        for r in runs:
            r.chain = runs[0].id
    db.commit()  # движок работает в своём потоке и должен увидеть запуски сразу
    engine_of(request).submit([r.id for r in runs])
    return runs


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
    custom: dict[str, dict[str, Any]] = {}
    if module_id == "__all__":
        wanted = [m.spec.id for m in applicable(ctx)]  # порядок: пассивные, потом активные, зависимости раньше
    else:
        module = ctx.modules.get(module_id)
        if module is None:
            raise HTTPException(404, "Модуль не найден")
        if form.get("with_params"):
            raw = {fld.key: form.get(f"p_{fld.key}") for fld in module.spec.params}
            params, errors = f.parse_form(module.spec.params, raw)
        else:  # быстрый запуск ▶ — параметры по умолчанию (пустой чекбокс в форме значил бы «выключено»)
            params, errors = _defaults(module), {}
        if errors:
            labels = {fld.key: fld.label for fld in module.spec.params}
            flash(request, "Не запущено: " + "; ".join(f"{labels[k]} — {v}" for k, v in errors.items()), "error")
            return redirect(f"/targets/{target.id}")
        wanted, custom = [module.spec.id], {module.spec.id: params}
    chosen = plan(ctx, db, target.id, wanted)
    if not chosen:
        flash(request, "Нет модулей, которые можно запустить по этой цели", "info")
        return redirect(f"/targets/{target.id}")
    runs = _queue(request, db, target.id, [(m.spec.id, custom.get(m.spec.id) or _defaults(m)) for m in chosen])
    return redirect(f"/targets/{target.id}?run={runs[0].id}#console")


def stop_runs(ctx: Context, db, where) -> tuple[int, int]:
    """Остановить запуски по условию: ждущие — отменить сразу, идущие — сигналом движку.
    Возвращает (отменено до начала, останавливается)."""
    engine: Engine = ctx.services["engine"]
    # условие на статус в самом UPDATE (RETURNING — какие строки он реально поменял):
    # запуск, который движок успел начать между SELECT и UPDATE, сюда не попадёт
    waiting = list(db.scalars(update(Run).where(where, Run.status == "queued")
                              .values(status="cancelled", finished_at=utcnow()).returning(Run.id)))
    db.add_all(RunLine(run_id=i, n=1, level="warn", text="[stop] отменён до начала") for i in waiting)
    running = list(db.scalars(select(Run).where(where, Run.status == "running")))
    db.commit()  # движок должен увидеть отмену раньше, чем дойдёт до этих запусков
    stopping = 0
    for run in running:
        if engine.cancel(run.id):
            stopping += 1
        else:  # «идёт», но задачи в движке нет (движок упал) — честно закрываем сами
            run.status, run.finished_at, run.error = "cancelled", utcnow(), "остановлен: задача движка не найдена"
            n = (db.scalar(select(func.max(RunLine.n)).where(RunLine.run_id == run.id)) or 0) + 1
            db.add(RunLine(run_id=run.id, n=n, level="warn", text="[stop] остановлен: задача движка не найдена"))
    db.commit()
    return len(waiting), stopping


@router.post("/runs/{run_id}/cancel")
async def cancel(request: Request, db: DB, run_id: int):
    """Остановить запуск, а если он из цепочки — всю цепочку (only=1 — только этот запуск)."""
    run = _run_or_404(db, run_id)
    form = await form_data(request)
    back = str(form.get("back") or f"/runs/{run.id}")
    if not back.startswith("/") or back.startswith("//"):
        back = f"/runs/{run.id}"
    where = Run.chain == run.chain if run.chain is not None and not form.get("only") else Run.id == run.id
    waiting, stopping = stop_runs(get_ctx(request), db, where)
    if waiting or stopping:
        flash(request, "Останавливаю…" if stopping else "Запуск отменён", "info")
    return redirect(back)


@router.post("/runs/{run_id}/again")
async def again(request: Request, db: DB, run_id: int):
    """Повторить запуск — или всю его цепочку с теми же параметрами (only=1 — только этот)."""
    old = _run_or_404(db, run_id)
    only = bool((await form_data(request)).get("only"))
    group = [old] if old.chain is None or only else \
        list(db.scalars(select(Run).where(Run.chain == old.chain).order_by(Run.id)))
    runs = _queue(request, db, old.target_id, [(r.module_id, dict(r.params or {})) for r in group])
    return redirect(f"/targets/{old.target_id}?run={runs[0].id}#console")


def stop_target_runs(ctx: Context, db, target) -> None:
    """Цель удаляют — её запуски останавливаем: иначе модуль дописывал бы находки в никуда.
    Ничего не возвращает: удалению не мешает (вето — это строка с причиной)."""
    stop_runs(ctx, db, Run.target_id == target.id)


# --- живой вывод ---------------------------------------------------------------

def _clock(ctx: Context, when) -> str:
    try:
        tz = ZoneInfo(ctx.settings.get("general.timezone"))
    except (ZoneInfoNotFoundError, ValueError, TypeError):
        tz = timezone.utc
    return when.replace(tzinfo=timezone.utc).astimezone(tz).strftime("%H:%M:%S")


def module_title(ctx: Context, module_id: str) -> str:
    module = ctx.modules.get(module_id)
    return module.spec.title if module else module_id


def chain_of(db, run: Run) -> list[Run]:
    """Запуски цепочки по порядку; одиночный запуск — цепочка из себя самого."""
    if run.chain is None:
        return [run]
    return list(db.scalars(select(Run).where(Run.chain == run.chain).order_by(Run.id)))


LINES = 1000  # столько строк консоли отдаём за раз


def run_state(ctx: Context, db, run: Run, after: int = 0) -> dict[str, Any]:
    lines = list(db.scalars(select(RunLine).where(RunLine.run_id == run.id, RunLine.n > after)
                            .order_by(RunLine.n.desc()).limit(LINES + 1)))[::-1]
    hidden = 0
    if len(lines) > LINES:
        # длинный вывод: показываем хвост — там итог; сколько скрыто, пишем первой строкой
        hidden = (db.scalar(select(func.count()).where(RunLine.run_id == run.id, RunLine.n > after)) or 0) - LINES
        lines = lines[1:]
    members = chain_of(db, run)
    pos = next((i for i, r in enumerate(members, 1) if r.id == run.id), 1)
    nxt = members[pos] if pos < len(members) else None
    return {
        "id": run.id,
        "module": run.module_id,
        "module_title": module_title(ctx, run.module_id),
        "status": run.status,
        "title": run.state_title,
        "tone": run.state_tone,
        "active": run.active,
        "progress": run.progress,
        "seconds": round(run.seconds or 0, 1),
        "summary": run.summary,
        "error": run.error,
        "found_new": run.found_new,
        "pos": pos,
        "total": len(members),
        # следующий запуск цепочки: консоль переходит на него, когда этот закончится
        "next": {"id": nxt.id, "module_title": module_title(ctx, nxt.module_id), "pos": pos + 1} if nxt else None,
        "lines": ([{"n": lines[0].n - 1, "level": "dim", "text": f"… скрыто строк: {hidden} (вся запись — в журнале запуска)",
                    "at": _clock(ctx, lines[0].at)}] if hidden else [])
        + [{"n": ln.n, "level": ln.level, "text": ln.text, "at": _clock(ctx, ln.at)} for ln in lines],
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
    fields = {fld.key: fld for fld in module.spec.params} if module else {}
    params = [(fields[k].label, fields[k].display(v)) if k in fields else (k, str(v)) for k, v in (run.params or {}).items()]
    return render(request, "runs/detail.html", run=run, target=target, module=module, params=params,
                  view=console_view(get_ctx(request), db, run, whole_chain=False))


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
    view = console_view(ctx, db, console) if console is not None else None
    groups: dict[str, list] = {}
    for m in sorted(mods, key=lambda m: (m.spec.order, m.spec.id)):
        groups.setdefault(m.spec.category, []).append(m)
    return {"mods": mods, "groups": groups, "last": last, "console": view, "Kind": Kind,
            "titles": {m.spec.id: m.spec.title for m in ctx.modules},
            "nav_count": len(mods), "nav_tone": "warn" if view and view["active"] else ""}


def console_view(ctx: Context, db, run: Run, whole_chain: bool = True) -> dict[str, Any]:
    """Консоль запуска — или всей его цепочки: вывод модулей подряд, с разделителями, как в терминале."""
    members = chain_of(db, run) if whole_chain else [run]
    current = next((r for r in members if r.active), members[-1])
    parts = []
    for pos, r in enumerate(members, 1):
        if r.status == "queued" and r.id != current.id:
            continue  # ещё не начинался — появится, когда до него дойдёт очередь
        state = run_state(ctx, db, r)
        parts.append({"run": r, "pos": pos, "state": state})
    statuses = {r.status for r in members}
    if current.active:
        title, tone = current.state_title, current.state_tone
    elif "failed" in statuses:
        title, tone = "есть ошибки", "danger"
    elif statuses & {"cancelled", "interrupted"}:
        title, tone = "остановлено", "warning"
    elif "skipped" in statuses:
        title, tone = "готово, есть пропуски", "warning"
    else:
        title, tone = "готово", "success"
    cur = next(p["state"] for p in parts if p["run"].id == current.id)
    return {"run": run, "current": current, "current_title": cur["module_title"],
            "pos": next(p["pos"] for p in parts if p["run"].id == current.id),
            "parts": parts, "total": len(members), "active": current.active,
            "after": cur["lines"][-1]["n"] if cur["lines"] else 0, "title": title, "tone": tone,
            "progress": cur["progress"] if current.active else None,
            "seconds": round(sum(r.seconds or 0 for r in members), 1)}


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


def checked_column(db, targets) -> dict[str, Any] | None:
    """Колонка списка целей: когда по цели последний раз что-то отработало."""
    if not targets:
        return None
    last = dict(db.execute(select(Run.target_id, func.max(Run.finished_at))
                           .where(Run.target_id.in_([t.id for t in targets]), Run.finished_at.is_not(None))
                           .group_by(Run.target_id)).all())
    return {"title": "Проверена", "order": 90, "cells": {tid: {"when": when} for tid, when in last.items() if when}}


def search_assets(ctx: Context, db, q: str) -> list[SearchHit]:
    """«Где этот IP?» — поиск по находкам всех целей."""
    q = q.strip().lower()
    if len(q) < 3:
        return []
    rows = db.execute(select(Asset, Target).join(Target, Target.id == Asset.target_id)
                      .where(func.lower(Asset.value).like(f"%{q}%")).order_by(Asset.kind, Asset.value).limit(8)).all()
    hits = []
    for asset, target in rows:
        kind = ctx.asset_types.get(asset.kind)
        hits.append(SearchHit(asset.value, f"/targets/{target.id}#found", "Найдено",
                              f"{kind.title if kind else asset.kind} у цели {target.name}", kind.icon if kind else "search"))
    return hits


def _v2_chain(conn) -> None:
    """1 → 2: цепочки запусков — одно нажатие запускает модуль вместе с тем, на чём он построен."""
    columns = {row[1] for row in conn.exec_driver_sql("PRAGMA table_info(run)")}
    if "chain" not in columns:
        conn.exec_driver_sql("ALTER TABLE run ADD COLUMN chain INTEGER")
    conn.exec_driver_sql("CREATE INDEX IF NOT EXISTS ix_run_chain ON run (chain)")


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
    ctx.db.migration("runs", 2, _v2_chain)
    ctx.hook("targets.columns", checked_column)
    ctx.search(lambda db, q: search_assets(ctx, db, q))
    ctx.hook("target.before_delete", lambda db, target: stop_target_runs(ctx, db, target))
    ctx.hook("app.ready", on_ready)
    ctx.hook("app.stop", lambda _ctx: engine.stop())
