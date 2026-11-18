"""Веб-слой: помощники, которыми пользуются страницы расширений.

    from perimeter.web import DB, render, flash, redirect, get_ctx, actor

    @router.get("/reports")
    def reports(request: Request, db: DB):
        return render(request, "reports/list.html", reports=...)
"""

from __future__ import annotations

from typing import Annotated, Any, Iterator

from fastapi import Depends, Request
from fastapi.responses import RedirectResponse
from sqlalchemy.orm import Session

from ..core.context import Context


def get_ctx(request: Request) -> Context:
    return request.app.state.ctx


def get_db(request: Request) -> Iterator[Session]:
    with get_ctx(request).db.session() as session:
        yield session


# scope="function": сессия закрывается (и коммитит) сразу после обработчика, ДО отправки ответа.
# По умолчанию FastAPI закрывает её после ответа — и браузер успевал прийти по редиректу
# раньше, чем изменения попали в базу.
DB = Annotated[Session, Depends(get_db, scope="function")]


def render(request: Request, template: str, status_code: int = 200, **context: Any):
    return request.app.state.ui.render(request, template, context, status_code)


def flash(request: Request, message: str, tone: str = "success") -> None:
    """Всплывающее уведомление на следующей странице. tone: success | error | info | warning"""
    request.session.setdefault("_flash", []).append({"text": message, "tone": tone})


def pop_flashes(request: Request) -> list[dict[str, str]]:
    return request.session.pop("_flash", [])


def redirect(url: str) -> RedirectResponse:
    return RedirectResponse(url, status_code=303)


def actor(request: Request) -> str:
    user = getattr(request.state, "user", None)
    return user.username if user is not None else "system"


async def form_data(request: Request) -> dict[str, Any]:
    """Данные формы как dict; поля с несколькими значениями — списком."""
    form = await request.form()
    data: dict[str, Any] = {}
    for key in form.keys():
        values = form.getlist(key)
        data[key] = values if len(values) > 1 else values[0]
    return data
