"""Контракт модуля-инструмента.

Модуль — это класс с паспортом (`ModuleSpec`) и методом `run`. Паспорт
говорит панели всё, что ей нужно, ещё до запуска:

* `kind` — пассивный (только публичные источники, к цели не обращается)
  или активный (шлёт запросы цели — только к адресам внутри её границ);
* `consumes` / `produces` — типы данных на входе и выходе. По ним панель
  сама строит цепочки: модуль, который выдаёт `subdomain`, может кормить
  любой модуль, который `subdomain` принимает;
* `params` — параметры запуска, те же поля, что и в настройках.

    class DnsRecords(Module):
        spec = ModuleSpec(
            id="dns_records", title="DNS-записи", kind=Kind.PASSIVE,
            category="Разведка", consumes=("domain", "subdomain"),
            produces=("ip", "dns_record"),
            description="A, AAAA, MX, NS, TXT, CAA через публичные резолверы",
        )

        async def run(self, run: ModuleRun) -> None:
            ...

    def setup(ctx):
        ctx.modules.add(DnsRecords)

Сам движок запусков (очередь, лимиты, журнал) появится следующим этапом и
будет передавать в `run` объект, удовлетворяющий протоколу `ModuleRun`.
"""

from __future__ import annotations

from dataclasses import dataclass
from enum import Enum
from typing import Any, ClassVar, Protocol

from .fields import Field
from .registry import Registry, RegistryError


class Kind(str, Enum):
    PASSIVE = "passive"
    ACTIVE = "active"

    @property
    def title(self) -> str:
        return "Пассивный" if self is Kind.PASSIVE else "Активный"

    @property
    def hint(self) -> str:
        if self is Kind.PASSIVE:
            return "Берёт данные из публичных источников, к цели не обращается"
        return "Обращается к сайту — только к адресам внутри границ цели"


@dataclass(frozen=True)
class AssetType:
    """Тип данных, которыми обмениваются модули: домен, IP, порт, URL…"""

    id: str
    title: str
    icon: str = "box"
    description: str = ""
    order: int = 100


BUILTIN_ASSET_TYPES: tuple[AssetType, ...] = (
    AssetType("domain", "Домен", "globe", "Корневой домен цели", 10),
    AssetType("subdomain", "Поддомен", "git-branch", "Имя внутри домена цели", 20),
    AssetType("ip", "IP-адрес", "server", "IPv4 или IPv6", 30),
    AssetType("port", "Порт и сервис", "plug", "Открытый порт и что на нём отвечает", 40),
    AssetType("url", "URL", "link", "Адрес страницы или ресурса", 50),
    AssetType("technology", "Технология", "layers", "CMS, фреймворк, сервер, библиотека с версией", 60),
    AssetType("certificate", "Сертификат", "lock", "TLS-сертификат и его поля", 70),
    AssetType("dns_record", "DNS-запись", "list", "Запись зоны: MX, TXT, CAA…", 80),
    AssetType("email", "Почтовый адрес", "mail", "Адрес на домене цели", 90),
    AssetType("asn", "Сеть (ASN)", "network", "Автономная система и провайдер", 100),
)


@dataclass(frozen=True)
class ModuleSpec:
    id: str
    title: str
    kind: Kind
    description: str = ""
    category: str = "Разное"
    consumes: tuple[str, ...] = ("domain",)
    produces: tuple[str, ...] = ()
    params: tuple[Field, ...] = ()
    version: str = "1.0"
    icon: str = "puzzle"
    order: int = 100


class ModuleRun(Protocol):
    """Что движок даёт модулю при запуске (реализация — на этапе движка)."""

    params: dict[str, Any]

    def setting(self, key: str) -> Any: ...
    def log(self, message: str) -> None: ...
    async def emit_asset(self, type: str, value: str, **data: Any) -> None: ...
    async def emit_finding(self, severity: str, title: str, **data: Any) -> None: ...
    @property
    def cancelled(self) -> bool: ...


class Module:
    spec: ClassVar[ModuleSpec]

    async def run(self, run: ModuleRun) -> None:  # pragma: no cover — контракт
        raise NotImplementedError


class ModuleRegistry(Registry[type[Module]]):
    def __init__(self, asset_types: Registry[AssetType]) -> None:
        super().__init__("Модуль", key=lambda m: m.spec.id)
        self.asset_types = asset_types

    def add(self, item: type[Module]) -> type[Module]:
        spec = getattr(item, "spec", None)
        if not isinstance(spec, ModuleSpec):
            raise RegistryError(f"У модуля {item!r} нет паспорта ModuleSpec")
        return super().add(item)

    def __iter__(self):
        return iter(sorted(self._items.values(), key=lambda m: (m.spec.category, m.spec.order, m.spec.title)))

    def check(self) -> list[str]:
        """Проверка после загрузки всех расширений: известны ли типы данных."""
        problems = []
        for module in list(self._items.values()):
            unknown = [t for t in (*module.spec.consumes, *module.spec.produces) if t not in self.asset_types]
            if unknown:
                problems.append(f"Модуль «{module.spec.id}»: неизвестные типы данных {', '.join(unknown)}")
                self.remove(module.spec.id)
        return problems

    def followers(self, module_id: str) -> list[type[Module]]:
        """Модули, которые могут принять то, что выдаёт данный."""
        produced = set(self[module_id].spec.produces)
        return [m for m in self if m.spec.id != module_id and produced & set(m.spec.consumes)]

    def accepting(self, asset_type: str) -> list[type[Module]]:
        return [m for m in self if asset_type in m.spec.consumes]

    def categories(self) -> list[tuple[str, list[type[Module]]]]:
        result: dict[str, list[type[Module]]] = {}
        for m in self:
            result.setdefault(m.spec.category, []).append(m)
        return list(result.items())
