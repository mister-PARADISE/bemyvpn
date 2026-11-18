"""Расширяемость: своё расширение из data/plugins добавляет страницу, модуль, виджет и настройки;
сломанное — откатывается целиком и не роняет панель."""

from __future__ import annotations

from textwrap import dedent

from fastapi.testclient import TestClient

from .conftest import PASSWORD, Browser, make_app

GOOD_PLUGIN = '''
from fastapi import APIRouter, Request
from perimeter.core import fields as f
from perimeter.core.context import Page, Slot, Widget
from perimeter.core.modules import AssetType, Kind, Module, ModuleSpec
from perimeter.core.settings import SettingsSection
from perimeter.web import render

TITLE = "Пример"
VERSION = "0.1"
router = APIRouter()

@router.get("/hello")
def hello(request: Request):
    return render(request, "hello/page.html", who=request.app.state.ctx.settings.get("hello.who"))

class Whois(Module):
    spec = ModuleSpec(id="whois", title="WHOIS", kind=Kind.PASSIVE, category="Разведка",
                      consumes=("domain",), produces=("registrar",))

class Ports(Module):
    spec = ModuleSpec(id="ports", title="Порты", kind=Kind.ACTIVE, category="Сеть",
                      consumes=("ip",), produces=("port",))

class Resolve(Module):
    spec = ModuleSpec(id="resolve", title="DNS", kind=Kind.PASSIVE, category="Разведка",
                      consumes=("domain",), produces=("ip",))

class Headers(Module):
    spec = ModuleSpec(id="headers", title="Заголовки", kind=Kind.ACTIVE, category="Веб",
                      consumes=("domain",), produces=("url",))

def setup(ctx):
    ctx.asset_types.add(AssetType("registrar", "Регистратор"))
    ctx.pages.add(Page("hello", "Привет", "/hello", icon="star", section="Своё"))
    ctx.add_router(router)
    ctx.settings.section(SettingsSection(id="hello", title="Привет", group="Модули",
                                         fields=(f.text("who", "Кого", default="мир"),)))
    ctx.widgets.add(Widget("hello", "Привет", "hello/widget.html"))
    ctx.add_slot("target.side", Slot("hello", "hello/widget.html"))
    for m in (Whois, Ports, Resolve, Headers):
        ctx.modules.add(m)
'''

BROKEN_PLUGIN = '''
from perimeter.core.context import Page
def setup(ctx):
    ctx.pages.add(Page("broken", "Сломанное", "/broken"))
    raise RuntimeError("упало специально")
'''


def write_plugin(root, name, code, templates=None):
    pdir = root / "data" / "plugins" / name
    pdir.mkdir(parents=True)
    (pdir / "__init__.py").write_text(dedent(code), encoding="utf-8")
    for rel, text in (templates or {}).items():
        path = pdir / "templates" / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")


def login(app) -> Browser:
    b = Browser(TestClient(app))
    b.get("/setup")
    b.post("/setup", {"username": "admin", "password": PASSWORD, "password2": PASSWORD})
    b.get("/")
    return b


def test_plugin_extends_everything(tmp_path):
    write_plugin(tmp_path, "hello", GOOD_PLUGIN, {
        "hello/page.html": '{% extends "base.html" %}{% block content %}Привет, {{ who }}!{% endblock %}',
        "hello/widget.html": "<div class='card-body'>виджет-привет</div>",
    })
    write_plugin(tmp_path, "broken", BROKEN_PLUGIN)
    app = make_app(tmp_path)
    ctx = app.state.ctx

    info = {e.name: e for e in ctx.extensions}
    assert info["hello"].ok and info["hello"].version == "0.1"
    assert not info["broken"].ok and "упало специально" in info["broken"].error
    assert "broken" not in ctx.pages  # откат: страница сломанного расширения не осталась

    b = login(app)
    assert "Привет, мир!" in b.get("/hello").text
    assert "Привет" in b.get("/").text and "виджет-привет" in b.get("/").text
    b.post("/settings/hello", {"who": "Периметр"})
    assert "Привет, Периметр!" in b.get("/hello").text

    page = b.get("/modules").text
    assert "WHOIS" in page and "Порты" in page and "Активный" in page
    assert "упало специально" in b.get("/modules?tab=extensions").text
    assert [m.spec.id for m in ctx.modules.followers("resolve")] == ["ports"]

    b.post("/targets/new", {"domain": "example.com"})
    detail = b.get("/targets/1").text
    assert "WHOIS" in detail and "Заголовки" in detail and "Порты" not in detail  # Порты ждут IP, не домен
    assert detail.count("Нужно разрешение") == 1  # закрыт только активный модуль
    assert "виджет-привет" in detail  # вставка расширения в карточку цели
    b.post("/targets/1/authorize", {"method": "document", "note": "Договор"})
    assert "Нужно разрешение" not in b.get("/targets/1").text


def test_module_with_unknown_type_is_dropped(tmp_path):
    write_plugin(tmp_path, "odd", '''
from perimeter.core.modules import Kind, Module, ModuleSpec
class Odd(Module):
    spec = ModuleSpec(id="odd", title="Странный", kind=Kind.PASSIVE, consumes=("nonsense",))
def setup(ctx):
    ctx.modules.add(Odd)
''')
    ctx = make_app(tmp_path).state.ctx
    assert "odd" not in ctx.modules
    assert any(e.error and "nonsense" in e.error for e in ctx.extensions)
