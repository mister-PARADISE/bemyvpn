"""Ядро без веба: поля, настройки, границы, реестры, события."""

from __future__ import annotations

from datetime import date
from decimal import Decimal

import pytest

from perimeter.core import fields as f
from perimeter.core.db import Database
from perimeter.core.events import EventBus
from perimeter.core.fields import KEEP, FieldError
from perimeter.core.registry import Registry, RegistryError
from perimeter.core.scope import Scope, ScopeError, display_host, normalize_host, parse_rules
from perimeter.core.settings import Settings, SettingsSection


# --- поля ---------------------------------------------------------------------

def test_int_field_bounds_and_errors():
    fld = f.integer("n", "Число", default=5, min=1, max=10)
    assert fld.parse("7") == 7
    assert fld.parse(" 1 0 ") == 10
    with pytest.raises(FieldError, match="Не больше 10"):
        fld.parse("11")
    with pytest.raises(FieldError, match="целое"):
        fld.parse("abc")
    assert fld.parse("") is None


def test_required_text_and_length():
    fld = f.text("name", "Имя", required=True, max_length=3)
    with pytest.raises(FieldError, match="Обязательное"):
        fld.parse("  ")
    with pytest.raises(FieldError, match="Не длиннее 3"):
        fld.parse("abcd")
    assert fld.parse(" ab ") == "ab"


def test_bool_choice_lines():
    assert f.boolean("b", "Флаг").parse("on") is True
    assert f.boolean("b", "Флаг").parse(None) is False
    ch = f.choice("c", "Выбор", {"a": "А", "b": "Б"})
    assert ch.default == "a"
    with pytest.raises(FieldError):
        ch.parse("z")
    assert f.lines("l", "Строки").parse(" x \n\n y ") == ["x", "y"]


def test_secret_empty_means_keep():
    assert f.secret("k", "Ключ").parse("") is KEEP
    assert f.secret("k", "Ключ").parse("abc") == "abc"


def test_date_decimal_email():
    d = f.date("d", "Дата")
    assert d.parse("2026-11-18") == date(2026, 11, 18)
    assert d.parse("18.11.2026") == date(2026, 11, 18)
    with pytest.raises(FieldError):
        d.parse("завтра")
    m = f.decimal("m", "Сумма", min=0)
    assert m.parse("1 500,5") == Decimal("1500.50")
    with pytest.raises(FieldError):
        m.parse("-1")
    with pytest.raises(FieldError):
        f.email("e", "Почта").parse("not-an-email")
    assert f.email("e", "Почта").parse("") is None


def test_parse_form_collects_errors():
    fields = (f.text("a", "A", required=True), f.integer("b", "B", min=0), f.secret("s", "S"))
    values, errors = f.parse_form(fields, {"a": "", "b": "-5", "s": ""})
    assert set(errors) == {"a", "b"}
    assert "s" not in values


# --- настройки -----------------------------------------------------------------

@pytest.fixture
def settings(tmp_path):
    db = Database(f"sqlite:///{tmp_path / 's.db'}")
    db.migrate(["core"])
    s = Settings(db)
    s.section(SettingsSection(id="x", title="X", fields=(
        f.text("name", "Имя", default="Периметр"),
        f.integer("n", "Число", default=3, min=1),
        f.secret("key", "Ключ"),
        f.lines("hosts", "Хосты", default=["a"]),
    )))
    return s


def test_settings_defaults_and_save(settings):
    assert settings.get("x.name") == "Периметр"
    res = settings.save_form("x", {"name": "Новое", "n": "5", "key": "secret", "hosts": "b\nc"})
    assert res.ok and set(res.changed) == {"name", "n", "key", "hosts"}
    assert settings.get("x.n") == 5 and settings.get("x.hosts") == ["b", "c"]
    # пустой секрет не затирает сохранённый
    settings.save_form("x", {"name": "Новое", "n": "5", "key": "", "hosts": "b\nc"})
    assert settings.get("x.key") == "secret"
    settings.save_form("x", {"name": "Новое", "n": "5", "key": "", "key__clear": "1", "hosts": "b\nc"})
    assert settings.get("x.key") is None


def test_settings_errors_write_nothing(settings):
    res = settings.save_form("x", {"name": "Другое", "n": "0"})
    assert not res.ok and "n" in res.errors
    assert settings.get("x.name") == "Периметр"


def test_settings_default_is_not_shared_mutable(settings):
    settings.get("x.hosts").append("mutated")
    assert settings.get("x.hosts") == ["a"]


def test_settings_survive_restart(settings, tmp_path):
    settings.set("x.n", 9)
    fresh = Settings(settings.db)
    fresh.section(settings.sections["x"])
    assert fresh.get("x.n") == 9


def test_settings_reject_unknown_and_date_fields(settings):
    with pytest.raises(KeyError):
        settings.get("x.nope")
    with pytest.raises(ValueError):
        SettingsSection(id="bad", title="Bad", fields=(f.date("d", "Дата"),))


# --- границы -------------------------------------------------------------------

@pytest.mark.parametrize("raw,expected", [
    ("https://WWW.Example.com:443/path?q=1", "example.com"),
    ("example.com.", "example.com"),
    ("Пример.рф", "xn--e1afmkfd.xn--p1ai"),
    ("192.0.2.10", "192.0.2.10"),
    ("[2001:db8::1]", "2001:db8::1"),
])
def test_normalize_root(raw, expected):
    assert normalize_host(raw, strip_www=True) == expected


@pytest.mark.parametrize("raw", ["", "localhost", "exa mple.com", "-bad-.com", "1.2.3", "http://"])
def test_normalize_rejects(raw):
    with pytest.raises(ScopeError):
        normalize_host(raw)


def test_display_host_roundtrip():
    assert display_host("xn--e1afmkfd.xn--p1ai") == "пример.рф"


def test_scope_rules():
    scope = Scope.build("www.example.com", ["203.0.113.10", "198.51.100.0/24", "*.example-cdn.net"],
                        ["pay.example.com", "198.51.100.66"])
    assert scope.root == "example.com"
    allowed = ["example.com", "www.example.com", "a.b.example.com", "https://api.example.com/x",
               "203.0.113.10", "198.51.100.5", "img.example-cdn.net", "example-cdn.net"]
    denied = ["evil-example.com", "example.com.evil.net", "pay.example.com", "x.pay.example.com",
              "198.51.100.66", "203.0.113.11", "other.org", "localhost"]
    for host in allowed:
        assert scope.allows(host), host
    for host in denied:
        assert not scope.allows(host), host
    ok, reason = scope.explain("pay.example.com")
    assert not ok and "Исключено" in reason


def test_www_exclusion_does_not_exclude_whole_domain():
    scope = Scope.build("example.com", [], ["www.example.com"])
    assert scope.allows("shop.example.com")
    assert not scope.allows("www.example.com")


def test_parse_rules_reports_bad_lines():
    good, bad = parse_rules(["example.org", "not a host", "10.0.0.0/8"])
    assert good == ["example.org", "10.0.0.0/8"]
    assert len(bad) == 1 and "not a host" in bad[0]


# --- реестр и события ----------------------------------------------------------

class Item:
    def __init__(self, id, order=0):
        self.id, self.order = id, order


def test_registry_order_and_duplicates():
    r = Registry("Штука")
    r.add(Item("b", 20))
    r.add(Item("a", 10))
    r.add(Item("c", 10))
    assert r.ids() == ["a", "c", "b"]
    with pytest.raises(RegistryError, match="уже зарегистрирован"):
        r.add(Item("a"))


def test_event_bus_patterns_and_isolation():
    bus = EventBus()
    seen = []
    bus.on("client.created", lambda e: seen.append(("exact", e["id"])))
    bus.on("client.*", lambda e: seen.append(("prefix", e.name)))
    bus.on("*", lambda e: seen.append(("all", e.name)))
    bus.on("client.created", lambda e: 1 / 0)  # падение не мешает остальным
    bus.emit("client.created", id=7)
    bus.emit("target.created", id=1)
    assert ("exact", 7) in seen and ("prefix", "client.created") in seen
    assert ("all", "target.created") in seen
    assert ("prefix", "target.created") not in seen


# --- после ревью: адреса, IDNA, слишком широкие правила, пустые настройки ------------

@pytest.mark.parametrize("raw,expected", [
    ("2001:db8::1", "2001:db8::1"),  # голый IPv6 (urlsplit читал бы хост «2001»)
    ("https://[2001:DB8::1]:8443/x", "2001:db8::1"),
    ("straße.de", "xn--strae-oqa.de"),  # IDNA 2008, как в браузерах; не чужой strasse.de
])
def test_normalize_ipv6_and_idna2008(raw, expected):
    assert normalize_host(raw) == expected


def test_include_rules_cannot_swallow_the_internet():
    from perimeter.core.scope import ScopeRule, check_include

    good, bad = parse_rules(["203.0.113.10/0", "10.0.0.0/24", "github.io", "shop.github.io"], include=True)
    assert good == ["10.0.0.0/24", "shop.github.io"]
    assert "0.0.0.0/0" in bad[0] and "широкая" in bad[0] and "общий домен" in bad[1]
    check_include(ScopeRule.parse("2001:db8::/48"))  # /48 для IPv6 ещё можно
    assert parse_rules(["10.0.0.0/8"])[0] == ["10.0.0.0/8"]  # в «не трогать» широкие сети можно


def test_empty_setting_means_default(settings):
    settings.section(SettingsSection(id="t", title="T", fields=(f.integer("hours", "Часы", default=12),)))
    assert settings.save_form("t", {"hours": ""}).ok
    assert settings.get("t.hours") == 12  # а не None, на котором падала бы арифметика


@pytest.mark.parametrize("value", ["nan", "inf", "-inf"])
def test_float_rejects_non_finite(value):
    with pytest.raises(FieldError):
        f.number("x", "X").parse(value)


def test_form_value_keeps_raw_text_after_error():
    assert f.number("x", "X").form_value("abc") == "abc"
    assert f.lines("l", "L").form_value("a\nb") == "a\nb"
