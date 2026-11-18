"""Настройки: расширения объявляют разделы, панель сама рисует формы.

    ctx.settings.section(SettingsSection(
        id="shodan", title="Shodan", group="Модули",
        fields=(f.secret("api_key", "Ключ API"),),
    ))
    ctx.settings.get("shodan.api_key")

Значение, которое ни разу не сохраняли, — это default из описания поля.
"""

from __future__ import annotations

import copy
from dataclasses import dataclass, field
from typing import Any, Mapping

from sqlalchemy import delete, select

from .db import Database
from .fields import KEEP, NON_JSON_KINDS, Field, FieldError
from .models import SettingRow
from .registry import Registry

_MISSING = object()


@dataclass(frozen=True)
class SettingsSection:
    id: str
    title: str
    fields: tuple[Field, ...] = ()
    description: str = ""
    icon: str = "sliders"
    group: str = "Панель"
    order: int = 100
    # Дополнительный шаблон под формой (например, смена пароля).
    template: str | None = None

    def __post_init__(self) -> None:
        keys = [f.key for f in self.fields]
        if len(keys) != len(set(keys)):
            raise ValueError(f"В разделе «{self.id}» повторяются ключи полей")
        for fld in self.fields:
            if fld.kind in NON_JSON_KINDS:
                raise ValueError(f"Поле «{self.id}.{fld.key}»: тип {fld.kind} в настройках не поддерживается")

    def field(self, key: str) -> Field:
        for f in self.fields:
            if f.key == key:
                return f
        raise KeyError(f"Настройка «{self.id}.{key}» не объявлена")


@dataclass
class SaveResult:
    errors: dict[str, str] = field(default_factory=dict)
    changed: dict[str, tuple[Any, Any]] = field(default_factory=dict)

    @property
    def ok(self) -> bool:
        return not self.errors


class Settings:
    """Реестр разделов + хранилище значений с кэшем в памяти."""

    def __init__(self, db: Database) -> None:
        self.db = db
        self.sections: Registry[SettingsSection] = Registry("Раздел настроек")
        self._cache: dict[str, Any] | None = None

    def section(self, section: SettingsSection) -> SettingsSection:
        return self.sections.add(section)

    def groups(self) -> list[tuple[str, list[SettingsSection]]]:
        result: dict[str, list[SettingsSection]] = {}
        for s in self.sections:
            result.setdefault(s.group, []).append(s)
        return list(result.items())

    # --- чтение --------------------------------------------------------
    def _values(self) -> dict[str, Any]:
        if self._cache is None:
            with self.db.session() as s:
                self._cache = {row.key: row.value for row in s.scalars(select(SettingRow))}
        return self._cache

    def _field(self, key: str) -> tuple[SettingsSection, Field]:
        section_id, _, field_key = key.partition(".")
        section = self.sections.get(section_id)
        if section is None:
            raise KeyError(f"Раздел настроек «{section_id}» не объявлен")
        return section, section.field(field_key)

    def get(self, key: str, default: Any = _MISSING) -> Any:
        try:
            _, fld = self._field(key)
        except KeyError:
            if default is _MISSING:
                raise
            return default
        values = self._values()
        if key in values:
            return copy.deepcopy(values[key])
        return copy.deepcopy(fld.default)

    def is_set(self, key: str) -> bool:
        return self._values().get(key) not in (None, "", [])

    def values(self, section_id: str) -> dict[str, Any]:
        section = self.sections[section_id]
        return {f.key: self.get(f"{section_id}.{f.key}") for f in section.fields}

    # --- запись --------------------------------------------------------
    def set(self, key: str, value: Any) -> None:
        """Программная запись одного значения (с проверкой типа)."""
        _, fld = self._field(key)
        self._write({key: fld.validate(value)}, [])

    def save_form(self, section_id: str, form: Mapping[str, Any]) -> SaveResult:
        """Сохраняет раздел целиком из данных формы. Ничего не пишет, если есть ошибки."""
        section = self.sections[section_id]
        result = SaveResult()
        updates: dict[str, Any] = {}
        clears: list[str] = []
        for fld in section.fields:
            full = f"{section_id}.{fld.key}"
            raw = form.get(fld.key)
            if fld.kind == "secret" and form.get(f"{fld.key}__clear"):
                if self.is_set(full):
                    clears.append(full)
                    result.changed[fld.key] = ("задан", "не задан")
                continue
            try:
                value = fld.parse(raw)
            except FieldError as exc:
                result.errors[fld.key] = str(exc)
                continue
            if value is KEEP:
                continue
            old = self.get(full)
            if value != old:
                if fld.kind == "secret":
                    result.changed[fld.key] = ("скрыто", "новое значение")
                else:
                    result.changed[fld.key] = (fld.display(old), fld.display(value))
            updates[full] = value
        if result.ok:
            self._write(updates, clears)
        return result

    def _write(self, updates: dict[str, Any], clears: list[str]) -> None:
        with self.db.session() as s:
            for key, value in updates.items():
                row = s.get(SettingRow, key)
                if row is None:
                    s.add(SettingRow(key=key, value=value))
                else:
                    row.value = value
            if clears:
                s.execute(delete(SettingRow).where(SettingRow.key.in_(clears)))
        cache = self._values()
        cache.update(copy.deepcopy(updates))
        for key in clears:
            cache.pop(key, None)
