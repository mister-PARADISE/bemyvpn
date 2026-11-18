"""Защита форм от подделки запроса (CSRF).

Токен живёт в подписанной сессии. Формы шлют его полем `_csrf`, fetch-запросы —
заголовком `X-CSRF-Token`. Проверка подключена ко всем роутерам автоматически.
"""

from __future__ import annotations

import hmac
import secrets

from fastapi import HTTPException, Request

SAFE_METHODS = {"GET", "HEAD", "OPTIONS"}


def csrf_token(request: Request) -> str:
    token = request.session.get("_csrf")
    if not token:
        token = secrets.token_urlsafe(32)
        request.session["_csrf"] = token
    return token


async def verify_csrf(request: Request) -> None:
    if request.method in SAFE_METHODS:
        return
    expected = request.session.get("_csrf")
    sent = request.headers.get("x-csrf-token")
    if not sent:
        content_type = request.headers.get("content-type", "")
        if content_type.startswith(("application/x-www-form-urlencoded", "multipart/form-data")):
            sent = (await request.form()).get("_csrf")
    if not expected or not sent or not hmac.compare_digest(str(sent), str(expected)):
        raise HTTPException(status_code=403, detail="Форма устарела — обновите страницу и попробуйте снова")
