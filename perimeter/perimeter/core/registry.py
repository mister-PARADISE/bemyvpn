"""Универсальный реестр: страницы, модули, виджеты, разделы настроек — всё через него."""

from __future__ import annotations

from typing import Callable, Generic, Iterator, TypeVar

T = TypeVar("T")


class RegistryError(ValueError):
    pass


def _default_key(item) -> str:
    return getattr(item, "id")


class Registry(Generic[T]):
    """Упорядоченный реестр с уникальными id.

    Порядок обхода: по атрибуту `order` (если есть), затем по порядку регистрации.
    """

    def __init__(self, kind: str, key: Callable[[T], str] = _default_key):
        self.kind = kind
        self._key = key
        self._items: dict[str, T] = {}

    def add(self, item: T) -> T:
        key = self._key(item)
        if not key or not isinstance(key, str):
            raise RegistryError(f"{self.kind}: пустой или нестроковый id")
        if key in self._items:
            raise RegistryError(f"{self.kind} «{key}» уже зарегистрирован")
        self._items[key] = item
        return item

    def remove(self, key: str) -> T | None:
        return self._items.pop(key, None)

    def get(self, key: str) -> T | None:
        return self._items.get(key)

    def __getitem__(self, key: str) -> T:
        try:
            return self._items[key]
        except KeyError:
            raise KeyError(f"{self.kind} «{key}» не найден") from None

    def __contains__(self, key: object) -> bool:
        return key in self._items

    def __len__(self) -> int:
        return len(self._items)

    def __iter__(self) -> Iterator[T]:
        items = list(self._items.values())
        # sorted стабилен: при равном order остаётся порядок регистрации.
        return iter(sorted(items, key=lambda it: getattr(it, "order", 0)))

    def ids(self) -> list[str]:
        return [self._key(it) for it in self]
