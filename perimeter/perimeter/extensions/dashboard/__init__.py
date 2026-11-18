"""Обзор: главная страница собирается из виджетов, которые приносят расширения."""

from __future__ import annotations

import logging
from datetime import date

from fastapi import APIRouter, Request

from ...core.context import Context, Page
from ...web import DB, get_ctx, render

TITLE = "Обзор"
DESCRIPTION = "Главная страница из виджетов расширений"
VERSION = "1.0"

log = logging.getLogger("perimeter.dashboard")
router = APIRouter()

WEEKDAYS = ("Понедельник", "Вторник", "Среда", "Четверг", "Пятница", "Суббота", "Воскресенье")
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


def setup(ctx: Context) -> None:
    ctx.pages.add(Page("dashboard", "Обзор", "/", icon="grid", placement="home", order=0,
                       description="Главная: оплаты, разрешения, последние действия"))
    ctx.add_router(router)
