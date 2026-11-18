"""Обзор: главная страница собирается из виджетов, которые приносят расширения."""

from __future__ import annotations

import logging
import platform
from datetime import date

from fastapi import APIRouter, Request

from ... import __version__
from ...core.context import Context, Page, Widget
from ...core.db import utcnow
from ...web import DB, get_ctx, render
from ...web.ui import plural

TITLE = "Обзор"
DESCRIPTION = "Главная страница из виджетов расширений"
VERSION = "1.0"

log = logging.getLogger("perimeter.dashboard")
router = APIRouter()

WEEKDAYS = ("понедельник", "вторник", "среда", "четверг", "пятница", "суббота", "воскресенье")
MONTHS = ("января", "февраля", "марта", "апреля", "мая", "июня", "июля", "августа", "сентября", "октября",
          "ноября", "декабря")


@router.get("/")
def dashboard(request: Request, db: DB):
    ctx = get_ctx(request)
    widgets = []
    for widget in ctx.widgets:
        try:
            data = widget.data(db, ctx) if widget.data else {}
            widgets.append({"widget": widget, "data": data, "error": None})
        except Exception:  # noqa: BLE001 — сломанный виджет не ломает главную
            log.exception("Виджет %s упал", widget.id)
            widgets.append({"widget": widget, "data": {}, "error": "Не удалось собрать данные — подробности в логе"})
    today = date.today()
    return render(request, "dashboard/index.html", widgets=widgets,
                  today=f"{WEEKDAYS[today.weekday()]}, {today.day} {MONTHS[today.month - 1]}")


def _size(num: float) -> str:
    for unit in ("Б", "КБ", "МБ", "ГБ"):
        if num < 1024 or unit == "ГБ":
            return f"{num:.0f} {unit}" if unit == "Б" else f"{num:.1f} {unit}"
        num /= 1024
    return f"{num:.1f} ГБ"


def _uptime(started) -> str:
    seconds = int((utcnow() - started).total_seconds())
    days, rest = divmod(seconds, 86400)
    hours, rest = divmod(rest, 3600)
    minutes = rest // 60
    if days:
        return f"{days} {plural(days, 'день', 'дня', 'дней')} {hours} ч"
    if hours:
        return f"{hours} ч {minutes} мин"
    return f"{minutes} мин"


def system_widget(started):
    def data(db, ctx: Context) -> dict:
        base = ctx.config.data_dir / "perimeter.db"
        size = sum(p.stat().st_size for p in (base, base.with_name(base.name + "-wal")) if p.exists())
        broken = [e for e in ctx.extensions if not e.ok]
        return {
            "rows": [
                ("версия", __version__),
                ("работает", _uptime(started)),
                ("база", _size(size)),
                ("python", platform.python_version()),
                ("адрес", f"{ctx.config.host}:{ctx.config.port}"),
            ],
            "extensions": len(ctx.extensions) - len(broken),
            "broken": broken,
        }
    return data


def setup(ctx: Context) -> None:
    ctx.pages.add(Page("dashboard", "Обзор", "/", icon="grid", placement="home", order=0,
                       description="Главная: цели, модули, система, журнал"))
    ctx.add_router(router)
    ctx.widgets.add(Widget("system", "Система", "dashboard/_system.html", data=system_widget(utcnow()),
                           icon="server", order=30))
