"""Обзор: состояние машины (как htop), строки «Панели» от расширений, журнал.

Блоки приносят расширения: ctx.widgets — целые разделы, ctx.stats — строки
«Панели» («цели 4», «прокси 12 живых из 15»). Сам обзор ничего не знает
о том, что именно показывает.
"""

from __future__ import annotations

import logging
from datetime import date

from fastapi import APIRouter, Request

from ...core.context import Context, Page, Stat, Widget
from ...web import DB, get_ctx, render
from ...web.ui import ru_date
from . import system

TITLE = "Обзор"
DESCRIPTION = "Состояние машины и панели"
VERSION = "2.0"

log = logging.getLogger("perimeter.dashboard")
router = APIRouter()


@router.get("/")
def dashboard(request: Request, db: DB):
    ctx = get_ctx(request)
    widgets = []
    for widget in ctx.widgets:
        try:
            data = widget.data(db, ctx) if widget.data else {}
            widgets.append({"widget": widget, "data": data, "error": None})
        except Exception:  # noqa: BLE001 — сломанный блок не ломает главную
            log.exception("Виджет %s упал", widget.id)
            widgets.append({"widget": widget, "data": {}, "error": "не удалось собрать данные, подробности в логе"})
    snap = system.snapshot(ctx.config.data_dir)
    return render(request, "dashboard/index.html", widgets=widgets,
                  lines=[ru_date(date.today()), f"{snap['host']}, {snap['os']}"])


@router.get("/api/system")
def system_api(request: Request):
    return system.snapshot(get_ctx(request).config.data_dir)


def system_widget(db, ctx: Context) -> dict:
    return system.snapshot(ctx.config.data_dir)


def panel_widget(db, ctx: Context) -> dict:
    rows = []
    for stat in ctx.stats:
        try:
            value = stat.value(db, ctx)
        except Exception:  # noqa: BLE001
            log.exception("Строка %s упала", stat.id)
            value = ("ошибка", "err")
        text, tone = value if isinstance(value, tuple) else (value, "")
        rows.append({"label": stat.label, "text": text, "tone": tone, "link": stat.link})
    return {"rows": rows}


def extensions_stat(db, ctx: Context):
    broken = [e.name for e in ctx.extensions if not e.ok]
    if broken:
        return f"ошибка: {', '.join(broken)}", "err"
    return f"{len(ctx.extensions)} ok", "ok"


def setup(ctx: Context) -> None:
    ctx.pages.add(Page("dashboard", "Обзор", "/", icon="grid", placement="home", order=0,
                       description="Машина, панель, журнал"))
    ctx.add_router(router)
    ctx.widgets.add(Widget("system", "Система", "dashboard/_system.html", data=system_widget, order=0, span=3))
    ctx.widgets.add(Widget("panel", "Панель", "dashboard/_panel.html", data=panel_widget, order=10))
    ctx.stats.add(Stat("extensions", "расширения", extensions_stat, order=90, link="/modules?tab=extensions"))
