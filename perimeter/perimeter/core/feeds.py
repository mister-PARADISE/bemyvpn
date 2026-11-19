"""Данные модулей, которые устаревают сами по себе, и их обновление по расписанию.

Модуль «Уязвимости» живёт на базах NVD, CISA KEV, EPSS: каждый день в них появляются
новые записи. Расширение описывает такой источник одним Feed — откуда, как часто, какие
модули им пользуются, — и панель сама обновляет его в фоне. После обновления пассивные
модули из Feed.modules перепроверяют цели, где уже работали: к сайтам они не ходят,
поэтому это безопасно, а новая уязвимость в старой версии всплывёт без участия человека.

    async def refresh(job: FeedJob) -> str:
        job.log("[*] качаю каталог")
        ...
        job.changed, job.items = 12, 1786
        return "записей 1786, новых 12"

    ctx.feeds.add(Feed("vulndb.kev", "CISA KEV", refresh, every=24, modules=("cve",)))
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Any, Awaitable, Callable, Protocol

if TYPE_CHECKING:
    from .context import Context


class FeedJob(Protocol):
    """То, что видит refresh во время обновления."""

    ctx: "Context"
    changed: int  # сколько записей добавилось, изменилось или ушло: 0 — цели перепроверять незачем
    items: int  # сколько записей всего — для страницы «Модули → Данные»

    def log(self, text: str, level: str = "info") -> None: ...  # info | ok | warn | err | dim | found
    def setting(self, key: str) -> Any: ...

    @property
    def data_dir(self) -> Path: ...


@dataclass(frozen=True)
class Feed:
    """Источник данных модуля, который панель обновляет сама.

    every — раз в сколько часов обновлять: число или функция от ctx (например, из настроек); 0 — не обновлять
    самой, только кнопкой. modules — какие модули пользуются данными: после обновления, которое что-то
    изменило, пассивные из них перепроверяют цели. refresh(job) делает работу и возвращает итог одной строкой.
    """

    id: str
    title: str
    refresh: Callable[[FeedJob], Awaitable[str]]
    description: str = ""
    source: str = ""  # откуда берётся — показывается рядом с названием
    every: float | Callable[["Context"], float] = 24
    modules: tuple[str, ...] = ()
    group: str = ""
    order: int = 100
    timeout: int = 3600

    def hours(self, ctx: "Context") -> float:
        value = self.every(ctx) if callable(self.every) else self.every
        try:
            return max(0.0, float(value or 0))
        except (TypeError, ValueError):
            return 0.0
