"""Данные модулей: базы, которые панель обновляет сама, и перепроверка целей после обновления.

Источник описывает расширение (ctx.feeds.add(Feed(...)), см. core/feeds.py), а здесь — расписание,
кнопка «обновить сейчас», состояние каждого источника и вкладка «Модули → Данные».
"""

from __future__ import annotations

from datetime import datetime

from fastapi import APIRouter, HTTPException, Request
from sqlalchemy import select

from ...core import fields as f
from ...core.context import Context, Slot, Stat
from ...core.db import utcnow
from ...core.settings import SettingsSection
from ...web import actor, flash, get_ctx, redirect
from .models import FeedState
from .scheduler import SYSTEM, Scheduler

TITLE = "Данные модулей"
DESCRIPTION = "Базы модулей обновляются сами; после обновления цели перепроверяются"
VERSION = "1.0"

router = APIRouter()


def scheduler_of(request: Request) -> Scheduler:
    return get_ctx(request).services["feeds"]


def _start(request: Request, ids: list[str]) -> list[str]:
    ctx = get_ctx(request)
    sched = scheduler_of(request)
    ids = [i for i in ids if i not in sched.running]
    if ids:
        who = actor(request)
        ctx.services["engine"].spawn(lambda: sched.cycle(only=ids, actor=who))
    return ids


@router.post("/feeds/{feed_id}/refresh")
def refresh_one(request: Request, feed_id: str):
    ctx = get_ctx(request)
    feed = ctx.feeds.get(feed_id)
    if feed is None:
        raise HTTPException(404, "Таких данных нет")
    if _start(request, [feed.id]):
        flash(request, f"Обновляю «{feed.title}»", "info")
    else:
        flash(request, f"«{feed.title}» уже обновляются", "warning")
    return redirect("/modules?tab=data")


@router.post("/feeds/refresh")
def refresh_all(request: Request):
    ctx = get_ctx(request)
    started = _start(request, [feed.id for feed in ctx.feeds])
    flash(request, f"Обновляю: {len(started)}" if started else "Всё уже обновляется", "info" if started else "warning")
    return redirect("/modules?tab=data")


def _in(when: datetime | None) -> str:
    if when is None:
        return ""
    seconds = int((when - utcnow()).total_seconds())
    if seconds <= 60:
        return "сейчас"
    if seconds < 3600:
        return f"через {seconds // 60} мин"
    if seconds < 86400:
        return f"через {seconds // 3600} ч"
    return f"через {seconds // 86400} дн"


def data_slot(db, ctx: Context, **_):
    """Вкладка «Модули → Данные»: что обновляется само, когда обновлялось и что изменилось."""
    feeds = list(ctx.feeds)
    if not feeds:
        return None
    sched: Scheduler = ctx.services["feeds"]
    states = {s.id: s for s in db.scalars(select(FeedState))}
    rows = []
    for feed in feeds:
        state = states.get(feed.id)
        hours = feed.hours(ctx)
        running = feed.id in sched.running or (state is not None and state.status == "running")
        users = [ctx.modules.get(m).spec.title for m in feed.modules if ctx.modules.get(m)]
        rows.append({"feed": feed, "state": state, "running": running, "users": users,
                     "every": (f"раз в {hours:g} ч" if hours < 48 else f"раз в {hours / 24:g} дн") if hours else "только кнопкой",
                     "next": _in(sched.next_at(feed, state)) if not running else ""})
    return {"rows": rows, "auto": bool(ctx.settings.get("feeds.auto")), "recheck": bool(ctx.settings.get("feeds.recheck")),
            "running": any(r["running"] for r in rows)}


def feeds_stat(db, ctx: Context):
    """Строка «Панели» на обзоре: когда данные модулей обновлялись и всё ли в порядке."""
    feeds = list(ctx.feeds)
    if not feeds:
        return "нет"
    states = {s.id: s for s in db.scalars(select(FeedState))}
    failed = [feed.title for feed in feeds if states.get(feed.id) and states[feed.id].status == "failed"]
    if failed:
        return f"ошибка: {', '.join(failed[:2])}", "err"
    done = [s.ok_at for s in states.values() if s.ok_at]
    if not done:
        return "ещё не обновлялись", "warn" if ctx.settings.get("feeds.auto") else ""
    oldest = min(done)
    seconds = int((utcnow() - oldest).total_seconds())
    ago = f"{seconds // 3600} ч назад" if seconds >= 3600 else f"{max(1, seconds // 60)} мин назад"
    return f"{len(done)} из {len(feeds)} свежие, старейшие — {ago}"


def setup(ctx: Context) -> None:
    sched = Scheduler(ctx)
    ctx.services["feeds"] = sched
    ctx.add_router(router)
    ctx.add_slot("catalog.data", Slot("feeds", "feeds/_data.html", data=data_slot, order=10))
    ctx.stats.add(Stat("feeds", "данные модулей", feeds_stat, order=25, link="/modules?tab=data"))
    ctx.settings.section(SettingsSection(
        id="feeds", title="Данные модулей", icon="refresh", group="Модули", order=20,
        description="Базы, на которых работают модули (уязвимости, сроки поддержки версий), устаревают каждый день. "
                    "Панель обновляет их сама и перепроверяет цели. Что и когда обновлялось — «Модули → Данные».",
        fields=(
            f.boolean("auto", "Обновлять данные модулей сами, по расписанию", default=True),
            f.boolean("recheck", "После обновления перепроверять цели пассивными модулями", default=True,
                      help="Они не обращаются к сайтам — только сверяют уже найденное со свежими базами. "
                           "Активные модули (порты, технологии) сами не запускаются никогда"),
        ),
    ))
    ctx.hook("app.ready", lambda _ctx: sched.start())


__all__ = ["SYSTEM", "Scheduler", "setup"]
