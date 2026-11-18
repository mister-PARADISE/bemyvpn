"""Context — единственная точка входа для расширений.

Расширение — это модуль Python с функцией `setup(ctx)`. Всё, что оно умеет
добавить в панель, добавляется через ctx:

    def setup(ctx: Context) -> None:
        ctx.pages.add(Page("reports", "Отчёты", "/reports", icon="file"))
        ctx.add_router(router)
        ctx.widgets.add(Widget("risk", "Риски", "reports/_widget.html", data=...))
        ctx.settings.section(SettingsSection(...))
        ctx.modules.add(SomeModule)
        ctx.search(provider)
        ctx.entity("report", lambda id: f"/reports/{id}")
        ctx.events.on("target.consent", handler)
"""

from __future__ import annotations

import logging
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable

from fastapi import APIRouter
from sqlalchemy import event as sa_event
from sqlalchemy.orm import Session

from ..config import Config
from .db import Database
from .events import EventBus
from .models import AuditEntry
from .modules import BUILTIN_ASSET_TYPES, AssetType, ModuleRegistry
from .registry import Registry
from .settings import Settings

log = logging.getLogger("perimeter")


@dataclass(frozen=True)
class Page:
    """Пункт бокового меню и страница за ним."""

    id: str
    title: str
    path: str
    icon: str = "circle"
    section: str = "Работа"
    order: int = 100
    # home — первым пунктом, отдельно; main — в группах по section; footer — внизу панели.
    placement: str = "main"
    # Число рядом с пунктом меню (например, сколько запусков идёт сейчас).
    badge: Callable[[Session], int | None] | None = None
    badge_tone: str = "accent"
    description: str = ""

    def is_active(self, url_path: str) -> bool:
        if self.path == "/":
            return url_path == "/"
        return url_path == self.path or url_path.startswith(self.path.rstrip("/") + "/")


@dataclass(frozen=True)
class Widget:
    """Карточка на главной. data() получает сессию БД и возвращает контекст шаблона."""

    id: str
    title: str
    template: str
    data: Callable[[Session, "Context"], dict[str, Any]] | None = None
    icon: str = "circle"
    order: int = 100
    span: int = 1  # ширина в колонках сетки: 1, 2 или 3
    link: str | None = None


@dataclass(frozen=True)
class Slot:
    """Вставка в чужую страницу. Страница объявляет место: {{ slot("target.main", target=target) }},
    а любое расширение кладёт туда свой блок — не трогая код страницы.

    data(session, ctx, **kwargs) → дополнительный контекст шаблона.
    """

    id: str
    template: str
    data: Callable[..., dict[str, Any]] | None = None
    order: int = 100


@dataclass(frozen=True)
class Stat:
    """Строка в разделе «Панель» на обзоре: «цели  4», «прокси  12 живых из 15».

    value(session, ctx) → текст или (текст, оттенок: ok | warn | err).
    """

    id: str
    label: str
    value: Callable[[Session, "Context"], str | tuple[str, str]]
    order: int = 100
    link: str | None = None


@dataclass(frozen=True)
class SearchHit:
    title: str
    url: str
    kind: str
    subtitle: str = ""
    icon: str = "circle"


SearchProvider = Callable[[Session, str], list[SearchHit]]


@dataclass
class ExtensionInfo:
    name: str
    title: str
    origin: str  # builtin | plugin
    path: str = ""
    version: str = ""
    description: str = ""
    error: str | None = None

    @property
    def ok(self) -> bool:
        return self.error is None


@dataclass
class Context:
    config: Config
    db: Database
    events: EventBus = field(default_factory=EventBus)
    settings: Settings = field(init=False)
    pages: Registry[Page] = field(default_factory=lambda: Registry("Страница"))
    widgets: Registry[Widget] = field(default_factory=lambda: Registry("Виджет"))
    stats: Registry[Stat] = field(default_factory=lambda: Registry("Строка состояния"))
    asset_types: Registry[AssetType] = field(default_factory=lambda: Registry("Тип данных"))
    modules: ModuleRegistry = field(init=False)
    extensions: list[ExtensionInfo] = field(default_factory=list)
    routers: list[tuple[APIRouter, bool]] = field(default_factory=list)
    template_dirs: list[Path] = field(default_factory=list)
    static_dirs: dict[str, Path] = field(default_factory=dict)
    template_globals: dict[str, Any] = field(default_factory=dict)
    slots: dict[str, Registry[Slot]] = field(default_factory=dict)
    hooks: dict[str, list[Callable[..., Any]]] = field(default_factory=dict)
    # Общие объекты расширений: движок запусков, пул прокси… ctx.services["engine"]
    services: dict[str, Any] = field(default_factory=dict)
    _search: list[SearchProvider] = field(default_factory=list)
    _entities: dict[str, tuple[str, Callable[[int], str]]] = field(default_factory=dict)

    def __post_init__(self) -> None:
        self.settings = Settings(self.db)
        for t in BUILTIN_ASSET_TYPES:
            self.asset_types.add(t)
        self.modules = ModuleRegistry(self.asset_types)

    # --- регистрация -----------------------------------------------------
    def add_router(self, router: APIRouter, *, auth: bool = True) -> None:
        """auth=False — страница доступна без входа (вход, первичная настройка)."""
        self.routers.append((router, auth))

    def add_templates(self, path: Path | str) -> None:
        self.template_dirs.append(Path(path))

    def add_static(self, name: str, path: Path | str) -> None:
        """Статика расширения будет доступна по /static/ext/<name>/…"""
        if name in self.static_dirs:
            raise ValueError(f"Статика «{name}» уже подключена")
        self.static_dirs[name] = Path(path)

    def add_slot(self, name: str, slot: Slot) -> Slot:
        return self.slots.setdefault(name, Registry(f"Вставка в «{name}»")).add(slot)

    def hook(self, name: str, fn: Callable[..., Any]) -> Callable[..., Any]:
        """Крючок: в отличие от события, его ответ важен тому, кто спрашивает.

        ctx.hook("target.before_delete", check)  →  ctx.call_hooks(...) соберёт ответы всех.
        """
        self.hooks.setdefault(name, []).append(fn)
        return fn

    def call_hooks(self, name: str, *args: Any, **kwargs: Any) -> list[Any]:
        return [fn(*args, **kwargs) for fn in self.hooks.get(name, [])]

    def search(self, provider: SearchProvider) -> SearchProvider:
        self._search.append(provider)
        return provider

    def entity(self, kind: str, url: Callable[[int], str], title: str | None = None) -> None:
        """Как построить ссылку на объект: журнал и поиск показывают её сами."""
        self._entities[kind] = (title or kind, url)

    # --- использование ---------------------------------------------------
    def entity_url(self, kind: str | None, entity_id: int | None) -> str | None:
        if kind is None or entity_id is None or kind not in self._entities:
            return None
        return self._entities[kind][1](entity_id)

    def entity_title(self, kind: str | None) -> str:
        if kind and kind in self._entities:
            return self._entities[kind][0]
        return kind or "—"

    def entity_kinds(self) -> list[tuple[str, str]]:
        return [(k, v[0]) for k, v in self._entities.items()]

    def run_search(self, session: Session, query: str, limit: int = 20) -> list[SearchHit]:
        hits: list[SearchHit] = []
        for provider in self._search:
            try:
                hits.extend(provider(session, query))
            except Exception:  # noqa: BLE001 — сломанный поиск одного расширения не ломает остальные
                log.exception("Поиск расширения упал")
        return hits[:limit]

    def audit(
        self,
        session: Session,
        action: str,
        summary: str,
        *,
        entity: tuple[str, int] | None = None,
        data: dict[str, Any] | None = None,
        actor: str = "system",
    ) -> AuditEntry:
        """Запись в журнал + событие `action` после успешного commit."""
        kind, entity_id = entity if entity else (None, None)
        entry = AuditEntry(
            actor=actor, action=action, entity_type=kind, entity_id=entity_id, summary=summary, data=data or None
        )
        session.add(entry)

        def _emit(_session: Session) -> None:
            self.events.emit(action, entity_type=kind, entity_id=entity_id, summary=summary, actor=actor, data=data)

        sa_event.listen(session, "after_commit", _emit, once=True)
        return entry
