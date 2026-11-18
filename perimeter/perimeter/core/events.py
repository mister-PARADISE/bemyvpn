"""Шина событий: расширения узнают друг о друге, не импортируя друг друга.

    ctx.events.on("target.consent", handler)
    ctx.events.on("target.*", handler)      # все события целей
    ctx.events.on("*", handler)             # вообще всё
    ctx.events.emit("run.done", target_id=5)

Ошибка в одном обработчике не ломает остальные и не ломает того, кто событие
отправил: она пишется в лог.
"""

from __future__ import annotations

import asyncio
import inspect
import logging
from collections import defaultdict
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Any, Callable

log = logging.getLogger("perimeter.events")

Handler = Callable[["Event"], Any]


@dataclass(frozen=True)
class Event:
    name: str
    payload: dict[str, Any] = field(default_factory=dict)
    at: datetime = field(default_factory=lambda: datetime.now(timezone.utc))

    def __getitem__(self, key: str) -> Any:
        return self.payload[key]


class EventBus:
    def __init__(self) -> None:
        self._handlers: dict[str, list[Handler]] = defaultdict(list)

    def on(self, pattern: str, handler: Handler | None = None):
        """Подписка. Можно как вызов, можно как декоратор."""

        def register(fn: Handler) -> Handler:
            self._handlers[pattern].append(fn)
            return fn

        return register(handler) if handler is not None else register

    def off(self, pattern: str, handler: Handler) -> None:
        if handler in self._handlers.get(pattern, []):
            self._handlers[pattern].remove(handler)

    def _matching(self, name: str) -> list[Handler]:
        found: list[Handler] = []
        for pattern, handlers in self._handlers.items():
            if pattern == "*" or pattern == name:
                found.extend(handlers)
            elif pattern.endswith(".*") and name.startswith(pattern[:-1]):
                found.extend(handlers)
        return found

    def emit(self, name: str, **payload: Any) -> Event:
        event = Event(name, payload)
        for handler in self._matching(name):
            try:
                result = handler(event)
                if inspect.isawaitable(result):
                    try:
                        asyncio.get_running_loop().create_task(result)
                    except RuntimeError:  # нет цикла — выполняем на месте
                        asyncio.run(result)
            except Exception:  # noqa: BLE001 — чужой обработчик не должен ронять отправителя
                log.exception("Обработчик события %s упал", name)
        return event
