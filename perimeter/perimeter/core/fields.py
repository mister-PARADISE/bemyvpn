"""Типизированные поля: из них собираются и настройки, и параметры модулей.

Одно описание поля даёт сразу три вещи: форму в интерфейсе, проверку ввода
и значение по умолчанию. Добавить настройку = добавить одну строку.

    from perimeter.core import fields as f
    f.text("brand", "Название панели", default="Периметр")
    f.integer("timeout", "Таймаут, с", default=10, min=1, max=120)
    f.secret("api_key", "Ключ API")
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from datetime import date as _date
from datetime import datetime as _datetime
from decimal import Decimal, InvalidOperation
from typing import Any, Iterable, Mapping


class FieldError(ValueError):
    """Понятное человеку сообщение о неверном значении."""


class _Keep:
    """Маркер «оставить как было» — пустое поле секрета при сохранении."""

    def __repr__(self) -> str:
        return "KEEP"


KEEP = _Keep()

KINDS = ("text", "textarea", "secret", "int", "float", "decimal", "bool", "choice", "lines", "date", "email")
# Значения этих типов не ложатся в JSON как есть — в настройках они запрещены.
NON_JSON_KINDS = ("date", "decimal")
_TRUE = {"1", "on", "true", "yes", "да"}
_EMAIL = re.compile(r"^[^@\s]+@[^@\s]+\.[^@\s]+$")


@dataclass(frozen=True)
class Field:
    key: str
    label: str
    kind: str = "text"
    default: Any = None
    help: str = ""
    placeholder: str = ""
    required: bool = False
    min: float | None = None
    max: float | None = None
    max_length: int | None = None
    choices: tuple[tuple[str, str], ...] = ()
    restart: bool = False  # изменение вступит в силу после перезапуска
    width: str = "full"  # full | half — ширина в сетке формы
    show_if: str | None = None  # ключ поля-переключателя: поле видно, только когда он включён

    def __post_init__(self) -> None:
        if self.kind not in KINDS:
            raise ValueError(f"Неизвестный тип поля {self.kind!r} у «{self.key}»")
        if not self.key.isidentifier():
            raise ValueError(f"Ключ поля должен быть идентификатором: {self.key!r}")

    # --- ввод из формы -------------------------------------------------
    def parse(self, raw: Any) -> Any:
        """Сырое значение из формы → проверенное значение (или KEEP для секрета)."""
        if self.kind == "bool":
            if isinstance(raw, bool):
                return raw
            return str(raw or "").strip().lower() in _TRUE
        if self.kind == "lines":
            if isinstance(raw, (list, tuple)):
                items = [str(x).strip() for x in raw]
            else:
                items = [line.strip() for line in str(raw or "").splitlines()]
            return self.validate([x for x in items if x])

        text = "" if raw is None else str(raw)
        if self.kind not in ("textarea", "secret"):
            text = text.strip()
        if self.kind == "secret" and text == "":
            return KEEP
        if text == "":
            if self.required:
                raise FieldError("Обязательное поле")
            return self.validate(None)
        # Сначала превращаем текст в число, потом проверяем границы: FieldError — тоже
        # ValueError, и общий try съел бы понятное «Не больше 10».
        if self.kind == "int":
            try:
                number = int(text.replace(" ", ""))
            except ValueError:
                raise FieldError("Нужно целое число") from None
            return self.validate(number)
        if self.kind == "float":
            try:
                number = float(text.replace(" ", "").replace(",", "."))
            except ValueError:
                raise FieldError("Нужно число") from None
            return self.validate(number)
        if self.kind == "decimal":
            try:
                amount = Decimal(text.replace(" ", "").replace(" ", "").replace(",", "."))
            except InvalidOperation:
                raise FieldError("Нужна сумма, например 1500 или 99.90") from None
            return self.validate(amount)
        if self.kind == "date":
            for fmt in ("%Y-%m-%d", "%d.%m.%Y"):
                try:
                    day = _datetime.strptime(text, fmt).date()
                except ValueError:
                    continue
                return self.validate(day)
            raise FieldError("Дата в формате ДД.ММ.ГГГГ")
        return self.validate(text)

    # --- проверка уже готового значения --------------------------------
    def validate(self, value: Any) -> Any:
        if value is None:
            if self.required:
                raise FieldError("Обязательное поле")
            return None
        if self.kind == "bool":
            return bool(value)
        if self.kind == "date":
            if not isinstance(value, _date):
                raise FieldError("Нужна дата")
            return value.date() if isinstance(value, _datetime) else value
        if self.kind == "decimal":
            try:
                value = Decimal(value)
            except (InvalidOperation, TypeError, ValueError):
                raise FieldError("Нужна сумма") from None
            if not value.is_finite():
                raise FieldError("Нужна сумма")
            if self.min is not None and value < Decimal(str(self.min)):
                raise FieldError(f"Не меньше {_num(self.min)}")
            if self.max is not None and value > Decimal(str(self.max)):
                raise FieldError(f"Не больше {_num(self.max)}")
            return value.quantize(Decimal("0.01"))
        if self.kind in ("int", "float"):
            if isinstance(value, bool) or not isinstance(value, (int, float)):
                raise FieldError("Нужно число")
            value = int(value) if self.kind == "int" else float(value)
            if self.min is not None and value < self.min:
                raise FieldError(f"Не меньше {_num(self.min)}")
            if self.max is not None and value > self.max:
                raise FieldError(f"Не больше {_num(self.max)}")
            return value
        if self.kind == "choice":
            value = str(value)
            if value not in dict(self.choices):
                raise FieldError("Выберите значение из списка")
            return value
        if self.kind == "lines":
            if isinstance(value, str):
                value = [value]
            items = [str(x) for x in value]
            limit = self.max_length or 1000
            if any(len(x) > limit for x in items):
                raise FieldError(f"Строка длиннее {limit} символов")
            return items
        value = str(value)
        limit = self.max_length or (20000 if self.kind == "textarea" else 500)
        if len(value) > limit:
            raise FieldError(f"Не длиннее {limit} символов")
        if self.kind == "email" and not _EMAIL.match(value):
            raise FieldError("Не похоже на почту")
        return value

    # --- вывод в форму -------------------------------------------------
    def form_value(self, value: Any) -> str:
        if self.kind == "secret" or value is None:
            return ""
        if self.kind == "lines":
            return "\n".join(value or [])
        if self.kind == "bool":
            return "1" if value else ""
        if self.kind == "float":
            return _num(value)
        if self.kind == "date":
            return value.isoformat()
        if self.kind == "decimal":
            return _num(Decimal(value))
        return str(value)

    def display(self, value: Any) -> str:
        """Короткое человеческое представление значения (для таблиц и журнала)."""
        if self.kind == "secret":
            return "задан" if value else "не задан"
        if self.kind == "bool":
            return "да" if value else "нет"
        if self.kind == "choice":
            return dict(self.choices).get(value, str(value))
        if self.kind == "lines":
            return ", ".join(value or []) or "—"
        if self.kind == "date" and value is not None:
            return value.strftime("%d.%m.%Y")
        return "—" if value in (None, "") else str(value)


def _num(x: float | Decimal) -> str:
    if isinstance(x, Decimal):
        return str(x.quantize(Decimal(1))) if x == x.to_integral_value() else format(x.normalize(), "f")
    return str(int(x)) if float(x).is_integer() else str(x)


def parse_form(fields: Iterable[Field], data: Mapping[str, Any]) -> tuple[dict[str, Any], dict[str, str]]:
    """Разбор формы по описанию полей: (значения, ошибки). Секрет без ввода не попадает в значения."""
    values: dict[str, Any] = {}
    errors: dict[str, str] = {}
    for fld in fields:
        try:
            value = fld.parse(data.get(fld.key))
        except FieldError as exc:
            errors[fld.key] = str(exc)
            continue
        if value is not KEEP:
            values[fld.key] = value
    return values, errors


def initial(fields: Iterable[Field], obj: Any = None) -> dict[str, Any]:
    """Начальные значения формы: из объекта, а чего нет — из default."""
    return {fld.key: getattr(obj, fld.key, fld.default) if obj is not None else fld.default for fld in fields}


def _choices(choices: Iterable[tuple[str, str]] | Mapping[str, str]) -> tuple[tuple[str, str], ...]:
    items = choices.items() if isinstance(choices, Mapping) else choices
    return tuple((str(k), str(v)) for k, v in items)


# --- фабрики: так поля читаются как анкета ---------------------------------

def text(key: str, label: str, default: str = "", **kw) -> Field:
    return Field(key, label, "text", default, **kw)


def textarea(key: str, label: str, default: str = "", **kw) -> Field:
    return Field(key, label, "textarea", default, **kw)


def secret(key: str, label: str, **kw) -> Field:
    return Field(key, label, "secret", None, **kw)


def integer(key: str, label: str, default: int | None = 0, **kw) -> Field:
    return Field(key, label, "int", default, **kw)


def number(key: str, label: str, default: float | None = 0.0, **kw) -> Field:
    return Field(key, label, "float", default, **kw)


def boolean(key: str, label: str, default: bool = False, **kw) -> Field:
    return Field(key, label, "bool", default, **kw)


def choice(key: str, label: str, choices, default: str | None = None, **kw) -> Field:
    opts = _choices(choices)
    if not opts:
        raise ValueError(f"У поля «{key}» нет вариантов")
    return Field(key, label, "choice", default if default is not None else opts[0][0], choices=opts, **kw)


def lines(key: str, label: str, default: Iterable[str] = (), **kw) -> Field:
    return Field(key, label, "lines", list(default), **kw)


def date(key: str, label: str, default: _date | None = None, **kw) -> Field:
    return Field(key, label, "date", default, **kw)


def email(key: str, label: str, default: str = "", **kw) -> Field:
    return Field(key, label, "email", default, **kw)


def decimal(key: str, label: str, default: Decimal | None = None, **kw) -> Field:
    return Field(key, label, "decimal", default, **kw)
