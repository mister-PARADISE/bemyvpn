"""Рендер страниц: окружение Jinja, фильтры, общий контекст для шаблонов."""

from __future__ import annotations

import logging
from datetime import date, datetime, timezone
from pathlib import Path
from typing import Any
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError

from fastapi import Request
from jinja2 import ChoiceLoader, Environment, FileSystemLoader, pass_context, select_autoescape
from markupsafe import Markup, escape
from starlette.templating import Jinja2Templates

from .. import __version__
from ..core.context import Context
from ..core.scope import display_host
from . import pop_flashes
from .csrf import csrf_token

log = logging.getLogger("perimeter.web")

WEB_DIR = Path(__file__).parent


WEEKDAYS = ("понедельник", "вторник", "среда", "четверг", "пятница", "суббота", "воскресенье")
MONTHS = ("января", "февраля", "марта", "апреля", "мая", "июня", "июля", "августа", "сентября", "октября",
          "ноября", "декабря")


def plural(n: int | float, one: str, few: str, many: str) -> str:
    n = abs(int(n))
    if n % 10 == 1 and n % 100 != 11:
        return one
    if 2 <= n % 10 <= 4 and not 12 <= n % 100 <= 14:
        return few
    return many


def ru_date(value: date, year: bool = False) -> str:
    """date(2026, 11, 18) → «среда, 18 ноября» (или «…18 ноября 2026»)."""
    text = f"{WEEKDAYS[value.weekday()]}, {value.day} {MONTHS[value.month - 1]}"
    return f"{text} {value.year}" if year else text



class SlotHTML(Markup):
    """HTML вставок плюс пункты оглавления (у Markup нет __dict__ — у наследника есть)."""

    nav: list[dict[str, Any]] = []

class UI:
    def __init__(self, ctx: Context) -> None:
        self.ctx = ctx
        loaders = [FileSystemLoader(str(WEB_DIR / "templates"))]
        loaders += [FileSystemLoader(str(p)) for p in ctx.template_dirs]
        env = Environment(
            loader=ChoiceLoader(loaders),
            autoescape=select_autoescape(["html", "xml", "svg"]),
            trim_blocks=True,
            lstrip_blocks=True,
        )
        env.filters.update(
            dt=self.fmt_datetime,
            date=self.fmt_date,
            ago=self.fmt_ago,
            clock=self.fmt_clock,
            host=display_host,
            nl2br=lambda s: Markup("<br>".join(escape(s or "").split("\n"))),
        )
        env.globals.update(
            plural=plural,
            icon=icon,
            ctx=ctx,
            version=__version__,
            slot=self.render_slot,
            partial=self.render_partial,
            now_line=self.now_line,
            **ctx.template_globals,
        )
        self.env = env
        self.templates = Jinja2Templates(env=env)

    def now_line(self) -> str:
        """«среда, 18 ноября 2026, 22:47» — в часовом поясе из настроек."""
        now = datetime.now(timezone.utc).astimezone(self.tz())
        return f"{ru_date(now.date(), year=True)}, {now:%H:%M}"

    @pass_context
    def render_partial(self, context, template: str, data: dict[str, Any] | None = None) -> Markup:
        """{{ partial("x/_widget.html", data) }} — шаблон с общим контекстом и ключами data как переменными."""
        return Markup(self.env.get_template(template).render({**context.get_all(), **(data or {})}))

    # --- вставки расширений ------------------------------------------------
    @pass_context
    def render_slot(self, context, name: str, **kwargs: Any) -> "SlotHTML":
        """{{ slot("target.main", target=target) }} — блоки всех расширений для этого места.

        У результата есть .nav — пункты оглавления от показанных блоков:
        {% set main = slot("target.main", ...) %} … {% for item in main.nav %} … {{ main }}
        """
        registry = self.ctx.slots.get(name)
        if not registry:
            return SlotHTML("")
        parts: list[str] = []
        nav: list[dict[str, Any]] = []
        with self.ctx.db.session() as session:
            for item in registry:
                try:
                    extra = item.data(session, self.ctx, **kwargs) if item.data else {}
                    if extra is None:  # блок решил не показываться
                        continue
                    template = self.env.get_template(item.template)
                    parts.append(template.render({**context.get_all(), **kwargs, **extra}))
                    if item.nav:
                        nav.append({"id": item.id, "title": item.nav, "count": extra.get("nav_count"),
                                    "tone": extra.get("nav_tone", "")})
                except Exception:  # noqa: BLE001 — чужой блок не роняет страницу
                    log.exception("Вставка %s в %s упала", item.id, name)
                    if self.ctx.config.debug:
                        raise
                    parts.append(
                        f'<div class="callout tone-danger">{icon("alert")}'
                        f"<div>Блок «{escape(item.id)}» не отрисовался — подробности в логе.</div></div>"
                    )
        out = SlotHTML("".join(parts))
        out.nav = nav
        return out

    # --- время -----------------------------------------------------------
    def tz(self) -> ZoneInfo | timezone:
        name = self.ctx.settings.get("general.timezone")
        try:
            return ZoneInfo(name)
        except (ZoneInfoNotFoundError, ValueError, TypeError):
            return timezone.utc

    def local(self, value: datetime) -> datetime:
        if value.tzinfo is None:
            value = value.replace(tzinfo=timezone.utc)
        return value.astimezone(self.tz())

    def fmt_clock(self, value: datetime | str | None) -> str:
        """Время строки журнала — в часовом поясе панели: 03:29:36. Принимает и ISO-строку (UTC)."""
        if isinstance(value, str):
            try:
                value = datetime.fromisoformat(value)
            except ValueError:
                return value
        if value is None:
            return ""
        return self.local(value).strftime("%H:%M:%S")

    def fmt_datetime(self, value: datetime | None) -> str:
        if value is None:
            return "—"
        return self.local(value).strftime("%d.%m.%Y %H:%M")

    def fmt_date(self, value: date | datetime | None) -> str:
        if value is None:
            return "—"
        if isinstance(value, datetime):
            value = self.local(value).date()
        return value.strftime("%d.%m.%Y")

    def fmt_ago(self, value: datetime | None) -> str:
        if value is None:
            return "—"
        if value.tzinfo is None:
            value = value.replace(tzinfo=timezone.utc)
        seconds = int((datetime.now(timezone.utc) - value).total_seconds())
        if seconds < 0:
            return self.fmt_datetime(value)
        if seconds < 60:
            return "только что"
        minutes = seconds // 60
        if minutes < 60:
            return f"{minutes} {plural(minutes, 'минуту', 'минуты', 'минут')} назад"
        hours = minutes // 60
        if hours < 24:
            return f"{hours} {plural(hours, 'час', 'часа', 'часов')} назад"
        days = hours // 24
        if days == 1:
            return "вчера"
        if days < 30:
            return f"{days} {plural(days, 'день', 'дня', 'дней')} назад"
        return self.fmt_date(value)

    # --- общий контекст --------------------------------------------------
    def nav(self, request: Request) -> dict[str, Any]:
        """Меню: home — отдельно сверху, sections — группы через разделитель, footer — внизу."""
        home: list[dict[str, Any]] = []
        footer: list[dict[str, Any]] = []
        sections: dict[str, list[dict[str, Any]]] = {}
        path = request.url.path
        with self.ctx.db.session() as session:
            for page in self.ctx.pages:
                badge = None
                if page.badge is not None:
                    try:
                        badge = page.badge(session)
                    except Exception:  # noqa: BLE001
                        log.exception("Счётчик страницы %s упал", page.id)
                item = {"page": page, "active": page.is_active(path), "badge": badge or None}
                if page.placement == "home":
                    home.append(item)
                elif page.placement == "footer":
                    footer.append(item)
                else:
                    sections.setdefault(page.section, []).append(item)
        return {"home": home, "sections": list(sections.items()), "footer": footer}

    def render(self, request: Request, template: str, context: dict[str, Any], status_code: int = 200):
        settings = self.ctx.settings
        nav = self.nav(request) if getattr(request.state, "user", None) else {"home": [], "sections": [], "footer": []}
        every = [*nav["home"], *(i for _, items in nav["sections"] for i in items), *nav["footer"]]
        active = next((item["page"] for item in every if item["active"]), None)
        has_session = "session" in request.scope
        base = {
            "user": getattr(request.state, "user", None),
            "nav": nav,
            "active_page": active,
            "brand": settings.get("general.brand_name"),
            "accent": settings.get("appearance.accent"),
            "density": settings.get("appearance.density"),
            "crt": settings.get("appearance.crt"),
            "nav_all": every,
            "flashes": pop_flashes(request) if has_session else [],
            "csrf": csrf_token(request) if has_session else "",
        }
        base.update(context)
        return self.templates.TemplateResponse(request, template, base, status_code=status_code)


def icon(name: str, cls: str = "") -> Markup:
    extra = f" {escape(cls)}" if cls else ""
    return Markup(f'<svg class="i{extra}" aria-hidden="true"><use href="#i-{escape(name)}"></use></svg>')
