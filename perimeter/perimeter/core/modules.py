"""Контракт модуля-инструмента.

Модуль — это класс с паспортом (`ModuleSpec`) и методом `run`. Паспорт
говорит панели всё, что ей нужно, ещё до запуска:

* `kind` — пассивный (открытые источники, к сайту не обращается) или
  активный (обращается к сайту — только к адресам, которые входят в цель);
* `consumes` / `produces` — типы данных на входе и выходе. По ним панель
  строит цепочки: модуль, который выдаёт `ip`, может кормить любой модуль,
  который `ip` принимает;
* `params` — параметры запуска, те же поля, что и в настройках;
* `timeout` — сколько секунд модулю можно работать.

    class Whois(Module):
        spec = ModuleSpec(id="whois", title="WHOIS", kind=Kind.PASSIVE, category="Разведка",
                          consumes=("domain",), produces=("dns_record",))

        async def run(self, run: ModuleRun) -> None:
            run.log(f"смотрю {run.target.root}")
            await run.emit("dns_record", "registrar REG.RU", registrar="REG.RU")

    def setup(ctx):
        ctx.modules.add(Whois)

Сеть активный модуль открывает только через `run.connect` / `run.tcp_ping`:
они проверяют, что адрес входит в цель, и отказывают иначе.
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
        return "Обращается к сайту — только к адресам, которые входят в цель"


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
    timeout: int = 300


class OutOfScope(PermissionError):
    """Адрес не входит в цель — модуль туда не пойдёт."""


class ModuleError(Exception):
    """Понятная человеку причина, почему модуль не смог отработать («список портов: не понял «8o»»)."""


class ModuleRun(Protocol):
    """Что движок даёт модулю при запуске (реализация — расширение «Запуски»)."""

    params: dict[str, Any]
    target: Any  # .id, .root, .name, .is_ip, .scope

    def setting(self, key: str) -> Any: ...
    def log(self, message: str, level: str = "info") -> None: ...  # info | ok | warn | err | dim | found
    def progress(self, done: int, total: int, label: str = "") -> None: ...
    async def emit(self, kind: str, value: str, **data: Any) -> bool: ...  # True — новая находка
    def assets(self, kind: str) -> list[dict[str, Any]]: ...  # что уже известно о цели
    def allows(self, host: str, port: int | None = None) -> tuple[bool, str]: ...
    async def connect(self, host: str, port: int, timeout: float = 3.0, **kw: Any) -> tuple[Any, Any]: ...
    async def tcp_ping(self, host: str, port: int, timeout: float = 3.0) -> float: ...
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
