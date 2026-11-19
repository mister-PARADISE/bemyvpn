"""Загрузка расширений: встроенных и своих из папки data/plugins.

Своё расширение — папка `data/plugins/<имя>/` с `__init__.py`, в котором есть
`setup(ctx)`. Рядом можно положить `templates/` и `static/` — подключатся сами.

Если setup() расширения падает, всё, что оно успело зарегистрировать,
откатывается, а ошибка видна в «Модули → Расширения». Панель продолжает работать.
"""

from __future__ import annotations

import importlib
import importlib.util
import logging
import sys
import traceback
from pathlib import Path
from types import ModuleType

from .context import Context, ExtensionInfo
from .registry import Registry

log = logging.getLogger("perimeter.extensions")

PLUGIN_NAMESPACE = "perimeter_plugins"


def load_all(ctx: Context) -> None:
    for name in (*ctx.config.builtin_extensions, *ctx.config.extra_extensions):
        _load(ctx, name, origin="builtin", importer=lambda n=name: importlib.import_module(n))
    plugins = ctx.config.plugins_dir
    if plugins.is_dir():
        for path in sorted(plugins.iterdir()):
            if path.name.startswith((".", "_")):
                continue
            if (path / "__init__.py").is_file():
                entry = path / "__init__.py"
            elif path.suffix == ".py" and path.is_file():
                entry = path
            else:
                continue
            name = f"{PLUGIN_NAMESPACE}.{path.stem}"
            _load(ctx, name, origin="plugin", importer=lambda e=entry, n=name: _import_file(n, e))
    for problem in ctx.modules.check():
        log.warning(problem)
        ctx.extensions.append(ExtensionInfo(name="modules", title="Проверка модулей", origin="builtin", error=problem))


def _import_file(name: str, entry: Path) -> ModuleType:
    if PLUGIN_NAMESPACE not in sys.modules:
        sys.modules[PLUGIN_NAMESPACE] = ModuleType(PLUGIN_NAMESPACE)
    is_pkg = entry.name == "__init__.py"
    spec = importlib.util.spec_from_file_location(
        name, entry, submodule_search_locations=[str(entry.parent)] if is_pkg else None
    )
    if spec is None or spec.loader is None:
        raise ImportError(f"Не удалось прочитать {entry}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    try:
        spec.loader.exec_module(module)
    except BaseException:
        sys.modules.pop(name, None)
        raise
    return module


def _load(ctx: Context, name: str, origin: str, importer) -> None:
    info = ExtensionInfo(name=name.rsplit(".", 1)[-1], title=name, origin=origin)
    taken = {e.name for e in ctx.extensions}
    ctx.extensions.append(info)
    if info.name in taken:
        # имя — это и компонент миграций: два «journal» делили бы одну версию схемы
        info.error = f"имя «{info.name}» уже занято другим расширением — переименуйте папку"
        log.error("Расширение %s не загружено: %s", name, info.error)
        return
    snapshot = _Snapshot(ctx)
    try:
        module = importer()
        info.title = getattr(module, "TITLE", info.name)
        info.description = getattr(module, "DESCRIPTION", "")
        info.version = getattr(module, "VERSION", "")
        info.path = str(Path(module.__file__ or "").parent)
        setup = getattr(module, "setup", None)
        if not callable(setup):
            raise TypeError("нет функции setup(ctx)")
        base = Path(module.__file__ or "").parent
        if (module.__file__ or "").endswith("__init__.py"):
            if (base / "templates").is_dir():
                ctx.add_templates(base / "templates")
            if (base / "static").is_dir():
                ctx.add_static(info.name, base / "static")
        setup(ctx)
    except Exception as exc:  # noqa: BLE001
        snapshot.rollback()
        info.error = f"{type(exc).__name__}: {exc}"
        log.error("Расширение %s не загружено:\n%s", name, traceback.format_exc())


class _Snapshot:
    """Запоминает состояние реестров, чтобы откатить неудачное расширение."""

    def __init__(self, ctx: Context) -> None:
        self.ctx = ctx
        self.registries: list[tuple[Registry, set[str]]] = [
            (r, set(r._items)) for r in (ctx.pages, ctx.widgets, ctx.stats, ctx.asset_types, ctx.modules, ctx.feeds,
                                         ctx.settings.sections,
                                         *ctx.slots.values())
        ]
        self.slot_names = set(ctx.slots)
        self.lists = [(lst, len(lst)) for lst in (ctx.routers, ctx.template_dirs, ctx._search)]
        self.statics = set(ctx.static_dirs)
        self.entities = set(ctx._entities)
        self.globals = set(ctx.template_globals)
        self.handlers = {k: list(v) for k, v in ctx.events._handlers.items()}
        self.hooks = {k: list(v) for k, v in ctx.hooks.items()}

    def rollback(self) -> None:
        for registry, keys in self.registries:
            for key in set(registry._items) - keys:
                registry.remove(key)
        for name in set(self.ctx.slots) - self.slot_names:
            del self.ctx.slots[name]
        for lst, size in self.lists:
            del lst[size:]
        for key in set(self.ctx.static_dirs) - self.statics:
            del self.ctx.static_dirs[key]
        for key in set(self.ctx._entities) - self.entities:
            del self.ctx._entities[key]
        for key in set(self.ctx.template_globals) - self.globals:
            del self.ctx.template_globals[key]
        self.ctx.events._handlers.clear()
        self.ctx.events._handlers.update(self.handlers)
        self.ctx.hooks.clear()
        self.ctx.hooks.update(self.hooks)
