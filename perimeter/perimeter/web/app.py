"""Сборка приложения: ядро → расширения → роутеры → шаблоны."""

from __future__ import annotations

import logging

from fastapi import APIRouter, Depends, FastAPI, Request
from fastapi.responses import JSONResponse
from fastapi.staticfiles import StaticFiles
from starlette.exceptions import HTTPException as StarletteHTTPException
from starlette.middleware.sessions import SessionMiddleware

from .. import __version__
from ..config import Config
from ..core import fields as f
from ..core.context import Context
from ..core.db import Database
from ..core.extensions import load_all
from ..core.settings import SettingsSection
from . import DB, get_ctx, render
from .auth import LoginRequired, account, login_redirect_url, require_user
from .auth import router as auth_router
from .csrf import verify_csrf
from .ui import WEB_DIR, UI

log = logging.getLogger("perimeter")

TIMEZONES = {
    "Europe/Kaliningrad": "Калининград (UTC+2)",
    "Europe/Moscow": "Москва (UTC+3)",
    "Europe/Samara": "Самара (UTC+4)",
    "Asia/Yekaterinburg": "Екатеринбург (UTC+5)",
    "Asia/Omsk": "Омск (UTC+6)",
    "Asia/Novosibirsk": "Новосибирск (UTC+7)",
    "Asia/Krasnoyarsk": "Красноярск (UTC+7)",
    "Asia/Irkutsk": "Иркутск (UTC+8)",
    "Asia/Yakutsk": "Якутск (UTC+9)",
    "Asia/Vladivostok": "Владивосток (UTC+10)",
    "Asia/Magadan": "Магадан (UTC+11)",
    "Asia/Kamchatka": "Камчатка (UTC+12)",
    "Europe/Minsk": "Минск (UTC+3)",
    "Europe/Kyiv": "Киев (UTC+2/+3)",
    "Asia/Almaty": "Алматы (UTC+5)",
    "Asia/Tashkent": "Ташкент (UTC+5)",
    "Asia/Tbilisi": "Тбилиси (UTC+4)",
    "Asia/Yerevan": "Ереван (UTC+4)",
    "Asia/Baku": "Баку (UTC+4)",
    "Europe/Istanbul": "Стамбул (UTC+3)",
    "Asia/Dubai": "Дубай (UTC+4)",
    "Europe/Berlin": "Берлин (UTC+1/+2)",
    "Europe/London": "Лондон (UTC+0/+1)",
    "America/New_York": "Нью-Йорк, Торонто (UTC−5/−4)",
    "America/Vancouver": "Ванкувер (UTC−8/−7)",
    "UTC": "UTC",
}

ACCENTS = {
    "green": "Зелёный люминофор",
    "amber": "Янтарный монитор",
    "cyan": "Бирюзовый",
}


def register_core(ctx: Context) -> None:
    ctx.settings.section(SettingsSection(
        id="general", title="Основное", icon="sliders", order=10,
        description="Как панель называется и в каком часовом поясе показывать время.",
        fields=(
            f.text("brand_name", "Название панели", default="Периметр", max_length=40),
            f.choice("timezone", "Часовой пояс", TIMEZONES, default="Europe/Moscow"),
            f.integer("remind_days", "Предупреждать заранее, дней", default=14, min=1, max=90,
                      help="За сколько дней до конца разрешения цель попадает в напоминания на «Обзоре»"),
        ),
    ))
    ctx.settings.section(SettingsSection(
        id="appearance", title="Внешний вид", icon="palette", order=20,
        description="Цвет терминала и плотность интерфейса.",
        fields=(
            f.choice("accent", "Цвет", ACCENTS, default="green"),
            f.choice("density", "Плотность", {"comfortable": "Свободно", "compact": "Компактно"}, default="comfortable"),
        ),
    ))
    ctx.settings.section(SettingsSection(
        id="security", title="Безопасность", icon="shield", order=30,
        description="Сколько живёт вход и смена пароля.",
        template="settings/_password.html",
        fields=(
            f.integer("session_hours", "Вход действует, часов", default=168, min=1, max=720,
                      help="После этого срока панель попросит пароль снова"),
        ),
    ))


def create_app(config: Config | None = None) -> FastAPI:
    config = config or Config.from_env()
    config.ensure_dirs()
    logging.basicConfig(level=logging.DEBUG if config.debug else logging.INFO,
                        format="%(asctime)s %(levelname)s %(name)s: %(message)s")

    ctx = Context(config=config, db=Database(config.db_url))
    register_core(ctx)
    load_all(ctx)
    components = ["core", *(e.name for e in ctx.extensions if e.ok)]
    ctx.db.migrate(components)

    app = FastAPI(title="Периметр", version=__version__, docs_url=None, redoc_url=None, openapi_url=None)
    app.state.ctx = ctx

    csrf = [Depends(verify_csrf)]
    app.include_router(auth_router, dependencies=csrf)
    app.include_router(account, dependencies=[Depends(require_user), *csrf])
    app.include_router(_api_router(), dependencies=[Depends(require_user), *csrf])
    for router, needs_auth in ctx.routers:
        deps = [Depends(require_user), *csrf] if needs_auth else csrf
        app.include_router(router, dependencies=deps)

    @app.get("/health", include_in_schema=False)
    def health():
        return {"ok": True, "version": __version__}

    app.mount("/static", StaticFiles(directory=WEB_DIR / "static"), name="static")
    for name, path in ctx.static_dirs.items():
        app.mount(f"/static/ext/{name}", StaticFiles(directory=path), name=f"static-{name}")

    app.state.ui = UI(ctx)
    _install_handlers(app)

    @app.middleware("http")
    async def security_headers(request: Request, call_next):
        response = await call_next(request)
        h = response.headers
        h.setdefault("X-Frame-Options", "DENY")
        h.setdefault("X-Content-Type-Options", "nosniff")
        h.setdefault("Referrer-Policy", "same-origin")
        h.setdefault(
            "Content-Security-Policy",
            "default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; "
            "script-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'",
        )
        if response.headers.get("content-type", "").startswith("text/html"):
            h.setdefault("Cache-Control", "no-store")
        return response

    app.add_middleware(
        SessionMiddleware,
        secret_key=config.secret_key(),
        session_cookie="perimeter_session",
        max_age=30 * 24 * 3600,
        same_site="lax",
        https_only=False,
    )
    return app


def _api_router() -> APIRouter:
    api = APIRouter(prefix="/api")

    @api.get("/search")
    def search(request: Request, db: DB, q: str = ""):
        ctx = get_ctx(request)
        query = q.strip()
        items = []
        low = query.lower()
        for page in ctx.pages:
            if not query or low in page.title.lower():
                items.append({"title": page.title, "url": page.path, "kind": "Раздел", "subtitle": page.description,
                              "icon": page.icon})
        for section in ctx.settings.sections:
            if query and (low in section.title.lower() or any(low in fl.label.lower() for fl in section.fields)):
                items.append({"title": section.title, "url": f"/settings/{section.id}", "kind": "Настройки",
                              "subtitle": section.description, "icon": section.icon})
        if len(query) >= 2:
            for hit in ctx.run_search(db, query):
                items.append(hit.__dict__)
        return {"items": items[:30]}

    return api


def _wants_json(request: Request) -> bool:
    return request.url.path.startswith("/api/") or "application/json" in request.headers.get("accept", "")


def _install_handlers(app: FastAPI) -> None:
    @app.exception_handler(LoginRequired)
    async def login_required(request: Request, exc: LoginRequired):
        if _wants_json(request):
            return JSONResponse({"error": "Нужно войти"}, status_code=401)
        from . import redirect
        return redirect(login_redirect_url(exc))

    @app.exception_handler(StarletteHTTPException)
    async def http_error(request: Request, exc: StarletteHTTPException):
        if _wants_json(request):
            return JSONResponse({"error": exc.detail}, status_code=exc.status_code)
        titles = {403: "Доступ запрещён", 404: "Страница не найдена", 405: "Так нельзя"}
        detail = exc.detail if exc.status_code != 404 else "Возможно, запись удалили или адрес набран с ошибкой."
        return render(request, "error.html", exc.status_code,
                      code=exc.status_code, title=titles.get(exc.status_code, "Ошибка"), detail=detail)

    @app.exception_handler(Exception)
    async def crash(request: Request, exc: Exception):
        log.exception("Ошибка при обработке %s", request.url.path)
        if _wants_json(request):
            return JSONResponse({"error": "Внутренняя ошибка"}, status_code=500)
        return render(request, "error.html", 500, code=500, title="Что-то сломалось",
                      detail="Подробности записаны в лог сервера.")
