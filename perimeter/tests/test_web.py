"""Панель целиком: вход, защита форм, цели и разрешения, настройки, поиск, страницы."""

from __future__ import annotations

from fastapi.testclient import TestClient
from sqlalchemy import select

from perimeter.core.models import AuditEntry, User
from perimeter.extensions.targets.models import Target

from .conftest import PASSWORD, Browser


def rows(ctx, model):
    with ctx.db.session() as s:
        return s.scalars(select(model)).all()


def fresh(app) -> Browser:
    b = Browser(TestClient(app))
    b.get("/login")
    return b


# --- вход ---------------------------------------------------------------------

def test_first_run_goes_to_setup(anon):
    r = anon.get("/targets", follow_redirects=False)
    assert r.status_code == 303 and r.headers["location"] == "/setup"


def test_setup_validates_password(anon):
    anon.get("/setup")
    r = anon.post("/setup", {"username": "admin", "password": "short", "password2": "short"})
    assert r.status_code == 422 and "не короче 10" in r.text


def test_setup_only_once(browser, app):
    b = fresh(app)
    r = b.post("/setup", {"username": "intruder", "password": PASSWORD, "password2": PASSWORD})
    assert r.headers["location"] == "/login"
    assert [u.username for u in rows(app.state.ctx, User)] == ["admin"]


def test_login_logout_and_next(browser, app):
    b = Browser(TestClient(app))
    r = b.get("/targets?state=pending", follow_redirects=False)
    assert r.headers["location"] == "/login?next=/targets%3Fstate%3Dpending"
    b.get("/login")
    r = b.post("/login", {"username": "admin", "password": "wrong-pass-1", "next": "/targets"})
    assert r.status_code == 401
    r = b.post("/login", {"username": "admin", "password": PASSWORD, "next": "/targets"})
    assert r.status_code == 303 and r.headers["location"] == "/targets"
    # при входе сессия создаётся заново (защита от фиксации сессии) — старый токен формы не годится
    assert b.post("/logout").status_code == 403
    b.get("/targets")
    assert b.post("/logout").status_code == 303
    assert b.get("/", follow_redirects=False).status_code == 303


def test_open_redirect_blocked(browser, app):
    b = fresh(app)
    r = b.post("/login", {"username": "admin", "password": PASSWORD, "next": "//evil.example"})
    assert r.headers["location"] == "/"


def test_login_throttle(browser, app):
    b = fresh(app)
    for _ in range(5):
        b.post("/login", {"username": "admin", "password": "nope-nope-1"})
    r = b.post("/login", {"username": "admin", "password": PASSWORD})
    assert r.status_code == 429


def test_csrf_required(browser):
    r = browser.client.post("/targets/new", data={"domain": "example.com"}, follow_redirects=False)
    assert r.status_code == 403
    r = browser.client.post("/targets/new", data={"domain": "example.com", "_csrf": "forged"}, follow_redirects=False)
    assert r.status_code == 403


def test_password_change_ends_other_sessions(browser, app, ctx):
    other = fresh(app)
    other.post("/login", {"username": "admin", "password": PASSWORD})
    assert other.get("/targets").status_code == 200
    r = browser.post("/account/password", {"current": PASSWORD, "new": "another-pass-2", "new2": "another-pass-2"})
    assert r.status_code == 303
    assert other.get("/targets", follow_redirects=False).status_code == 303
    assert browser.get("/targets").status_code == 200
    assert rows(ctx, User)[0].session_epoch == 1


# --- страницы ------------------------------------------------------------------

def test_every_registered_page_renders(browser, ctx):
    assert {p.id for p in ctx.pages} == {"dashboard", "targets", "modules", "journal", "settings"}
    for page in ctx.pages:
        r = browser.get(page.path)
        assert r.status_code == 200, (page.id, r.text[:500])
        assert 'class="cursor"' in r.text  # терминальная шапка на месте


def test_security_headers(browser):
    r = browser.get("/")
    assert r.headers["x-frame-options"] == "DENY"
    assert "default-src 'self'" in r.headers["content-security-policy"]
    assert r.headers["cache-control"] == "no-store"


def test_404_page(browser):
    r = browser.get("/targets/999")
    assert r.status_code == 404 and "Страница не найдена" in r.text


def test_static_font_served(browser):
    r = browser.client.get("/static/fonts/jetbrains-mono-cyrillic-wght-normal.woff2")
    assert r.status_code == 200 and r.content[:4] == b"wOF2"


# --- цели ----------------------------------------------------------------------

def test_target_normalization_and_duplicates(browser, ctx):
    r = browser.post("/targets/new", {"domain": "https://WWW.Пример.рф/shop"})
    assert r.status_code == 303
    target = rows(ctx, Target)[0]
    assert target.domain == "xn--e1afmkfd.xn--p1ai" and target.name == "пример.рф"
    r = browser.post("/targets/new", {"domain": "пример.рф"})
    assert r.status_code == 422 and "уже есть" in r.text
    r = browser.post("/targets/new", {"domain": "example.org", "include": "not a host"})
    assert r.status_code == 422 and "Не разобрал" in r.text
    r = browser.post("/targets/new", {"domain": "localhost"})
    assert r.status_code == 422


def test_consent_in_form_and_on_page(browser, ctx):
    browser.post("/targets/new", {"domain": "example.com"})
    t = rows(ctx, Target)[0]
    assert not t.consent and t.consent_at is None  # по умолчанию — только пассивные
    page = browser.get(f"/targets/{t.id}").text
    assert "Только пассивные" in page and "Владелец согласен" in page
    browser.post(f"/targets/{t.id}/consent", {"consent_note": "попросил в Telegram"})
    t = rows(ctx, Target)[0]
    assert t.consent and t.consent_note == "попросил в Telegram" and t.consent_at is not None
    assert "Все модули" in browser.get(f"/targets/{t.id}").text
    browser.post(f"/targets/{t.id}/consent/remove")
    t = rows(ctx, Target)[0]
    assert not t.consent and t.consent_at is None
    actions = [e.action for e in rows(ctx, AuditEntry)]
    assert "target.consent" in actions and "target.consent_removed" in actions


def test_consent_checkbox_on_create_and_edit(browser, ctx):
    browser.post("/targets/new", {"domain": "example.com", "consent": "1", "consent_note": "договор"})
    t = rows(ctx, Target)[0]
    assert t.consent and t.consent_note == "договор"
    browser.post(f"/targets/{t.id}/edit", {"domain": "example.com", "consent_note": "договор"})  # галочка снята
    assert not rows(ctx, Target)[0].consent
    assert "example.com" in browser.get("/targets?mode=no").text
    assert "example.com" not in browser.get("/targets?mode=yes").text


def test_scope_check_endpoint(browser, ctx):
    browser.post("/targets/new", {"domain": "example.com", "exclude": "pay.example.com"})
    tid = rows(ctx, Target)[0].id
    ok = browser.client.get(f"/targets/{tid}/scope", params={"host": "api.example.com"}).json()
    assert ok["allowed"] is True
    bad = browser.client.get(f"/targets/{tid}/scope", params={"host": "pay.example.com"}).json()
    assert bad["allowed"] is False and "Исключено" in bad["reason"]


def test_delete_target(browser, ctx):
    browser.post("/targets/new", {"domain": "example.com"})
    tid = rows(ctx, Target)[0].id
    browser.post(f"/targets/{tid}/delete")
    assert rows(ctx, Target) == []
    assert any(e.action == "target.deleted" for e in rows(ctx, AuditEntry))


# --- обзор, настройки, поиск -----------------------------------------------------

def test_dashboard_widgets(browser):
    browser.post("/targets/new", {"domain": "example.com"})
    page = browser.get("/").text
    for title in ("Цели", "Модули", "Система", "Журнал"):
        assert title in page
    assert "[+]" in page and "Новая цель example.com" in page


def test_settings_page_saves_and_audits(browser, ctx):
    r = browser.post("/settings/general", {"brand_name": "Моя панель", "timezone": "UTC"})
    assert r.status_code == 303
    assert ctx.settings.get("general.brand_name") == "Моя панель"
    assert "Моя панель" in browser.get("/").text
    assert any(e.action == "settings.updated" for e in rows(ctx, AuditEntry))
    r = browser.post("/settings/general", {"brand_name": "X" * 41, "timezone": "UTC"})
    assert r.status_code == 422
    assert ctx.settings.get("general.brand_name") == "Моя панель"


def test_accent_setting(browser, ctx):
    browser.post("/settings/appearance", {"accent": "amber", "density": "compact"})
    page = browser.get("/").text
    assert 'data-accent="amber"' in page and 'data-density="compact"' in page
    r = browser.post("/settings/appearance", {"accent": "pink", "density": "compact"})
    assert r.status_code == 422


def test_search(browser):
    browser.post("/targets/new", {"domain": "romashka.example", "title": "Ромашка"})
    items = browser.client.get("/api/search", params={"q": "ромаш"}).json()["items"]
    assert any(i["kind"] == "Цели" for i in items)  # кириллица без учёта регистра
    items = browser.client.get("/api/search", params={"q": "ROMASHKA"}).json()["items"]
    assert any(i["kind"] == "Цели" for i in items)
    items = browser.client.get("/api/search", params={"q": "часовой"}).json()["items"]
    assert any(i["kind"] == "Настройки" for i in items)
    assert browser.client.get("/api/search").json()["items"]  # пустой запрос — список разделов
