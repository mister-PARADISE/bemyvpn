"""Вход в панель: первичная настройка, логин, выход, смена пароля."""

from __future__ import annotations

import time
from urllib.parse import quote

from fastapi import APIRouter, Request
from sqlalchemy import func, select

from ..core.db import utcnow
from ..core.models import User
from ..core.security import LoginThrottle, hash_password, password_problem, verify_password
from . import DB, flash, form_data, get_ctx, redirect, render


class LoginRequired(Exception):
    def __init__(self, next_url: str = "/", setup: bool = False) -> None:
        self.next_url = next_url
        self.setup = setup


throttle = LoginThrottle()
router = APIRouter()


def _has_users(db) -> bool:
    return (db.scalar(select(func.count()).select_from(User)) or 0) > 0


def _safe_next(url: str | None) -> str:
    if not url or not url.startswith("/") or url.startswith("//"):
        return "/"
    return url


def _login(request: Request, user: User) -> None:
    request.session.clear()
    request.session.update({"uid": user.id, "epoch": user.session_epoch, "at": int(time.time())})


async def require_user(request: Request) -> User:
    """Зависимость всех закрытых страниц: кладёт пользователя в request.state.user."""
    ctx = get_ctx(request)
    uid = request.session.get("uid")
    with ctx.db.session() as db:
        user = db.get(User, uid) if uid else None
        if user is None and not _has_users(db):
            raise LoginRequired(setup=True)
    hours = ctx.settings.get("security.session_hours")
    fresh = time.time() - request.session.get("at", 0) < hours * 3600
    if user is None or request.session.get("epoch") != user.session_epoch or not fresh:
        request.session.pop("uid", None)
        path = request.url.path + (f"?{request.url.query}" if request.url.query else "")
        raise LoginRequired(next_url=path)
    request.state.user = user
    return user


@router.get("/setup")
def setup_form(request: Request, db: DB):
    if _has_users(db):
        return redirect("/login")
    return render(request, "auth/setup.html", form={}, errors={})


@router.post("/setup")
async def setup_submit(request: Request, db: DB):
    if _has_users(db):
        return redirect("/login")
    data = await form_data(request)
    username = str(data.get("username", "")).strip()
    password = str(data.get("password", ""))
    errors = {}
    if not 2 <= len(username) <= 64:
        errors["username"] = "От 2 до 64 символов"
    if problem := password_problem(password):
        errors["password"] = problem
    elif password != data.get("password2"):
        errors["password2"] = "Пароли не совпадают"
    if errors:
        return render(request, "auth/setup.html", status_code=422, form={"username": username}, errors=errors)
    user = User(username=username, password_hash=hash_password(password), last_login_at=utcnow())
    db.add(user)
    db.flush()
    get_ctx(request).audit(db, "auth.setup", f"Создан администратор {username}", actor=username)
    _login(request, user)
    flash(request, "Панель готова к работе")
    return redirect("/")


@router.get("/login")
def login_form(request: Request, db: DB, next: str = "/"):
    if not _has_users(db):
        return redirect("/setup")
    return render(request, "auth/login.html", next=_safe_next(next), error=None, username="")


@router.post("/login")
async def login_submit(request: Request, db: DB):
    data = await form_data(request)
    username = str(data.get("username", "")).strip()
    next_url = _safe_next(data.get("next"))
    key = request.client.host if request.client else "?"
    wait = throttle.retry_after(key)
    if wait:
        error = f"Слишком много попыток. Подождите {wait} с."
        return render(request, "auth/login.html", 429, next=next_url, error=error, username=username)
    user = db.scalar(select(User).where(User.username == username))
    if user is None or not verify_password(str(data.get("password", "")), user.password_hash):
        throttle.fail(key)
        return render(request, "auth/login.html", 401, next=next_url, error="Неверное имя или пароль", username=username)
    throttle.reset(key)
    user.last_login_at = utcnow()
    get_ctx(request).audit(db, "auth.login", f"Вход: {username}", actor=username, data={"ip": key})
    _login(request, user)
    return redirect(next_url)


@router.post("/logout")
async def logout(request: Request):
    request.session.clear()
    return redirect("/login")


account = APIRouter()


@account.post("/account/password")
async def change_password(request: Request, db: DB):
    data = await form_data(request)
    user = db.get(User, request.state.user.id)
    if not verify_password(str(data.get("current", "")), user.password_hash):
        flash(request, "Текущий пароль указан неверно", "error")
        return redirect("/settings/security")
    new = str(data.get("new", ""))
    if problem := password_problem(new):
        flash(request, problem, "error")
        return redirect("/settings/security")
    if new != data.get("new2"):
        flash(request, "Новые пароли не совпадают", "error")
        return redirect("/settings/security")
    user.password_hash = hash_password(new)
    user.session_epoch += 1
    get_ctx(request).audit(db, "auth.password", "Пароль изменён, остальные сессии закрыты", actor=user.username)
    _login(request, user)
    flash(request, "Пароль изменён. Все другие входы завершены")
    return redirect("/settings/security")


def login_redirect_url(exc: LoginRequired) -> str:
    if exc.setup:
        return "/setup"
    return "/login" if exc.next_url == "/" else f"/login?next={quote(exc.next_url)}"
