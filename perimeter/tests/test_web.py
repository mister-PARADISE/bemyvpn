"""Панель целиком: вход, защита форм, клиенты, счета, цели, настройки, страницы."""

from __future__ import annotations

from datetime import date, timedelta

from sqlalchemy import select

from perimeter.core.models import AuditEntry, User
from perimeter.extensions.clients.models import Client, Invoice
from perimeter.extensions.targets.models import Target

from .conftest import PASSWORD


def iso(days: int) -> str:
    return (date.today() + timedelta(days=days)).isoformat()


def rows(ctx, model):
    with ctx.db.session() as s:
        return s.scalars(select(model)).all()


# --- вход ---------------------------------------------------------------------

def test_first_run_goes_to_setup(anon):
    r = anon.get("/clients", follow_redirects=False)
    assert r.status_code == 303 and r.headers["location"] == "/setup"


def test_setup_validates_password(anon):
    anon.get("/setup")
    r = anon.post("/setup", {"username": "admin", "password": "short", "password2": "short"})
    assert r.status_code == 422 and "не короче 10" in r.text


def test_login_logout_and_next(browser, app):
    from fastapi.testclient import TestClient

    from .conftest import Browser

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
    from fastapi.testclient import TestClient

    from .conftest import Browser

    b = Browser(TestClient(app))
    b.get("/login")
    r = b.post("/login", {"username": "admin", "password": PASSWORD, "next": "//evil.example"})
    assert r.headers["location"] == "/"


def test_login_throttle(browser, app):
    from fastapi.testclient import TestClient

    from .conftest import Browser

    b = Browser(TestClient(app))
    b.get("/login")
    for _ in range(5):
        b.post("/login", {"username": "admin", "password": "nope-nope-1"})
    r = b.post("/login", {"username": "admin", "password": PASSWORD})
    assert r.status_code == 429


def test_csrf_required(browser):
    r = browser.client.post("/clients/new", data={"name": "X"}, follow_redirects=False)
    assert r.status_code == 403
    r = browser.client.post("/clients/new", data={"name": "X", "_csrf": "forged"}, follow_redirects=False)
    assert r.status_code == 403


def test_password_change_ends_other_sessions(browser, app, ctx):
    from fastapi.testclient import TestClient

    from .conftest import Browser

    other = Browser(TestClient(app))
    other.get("/login")
    other.post("/login", {"username": "admin", "password": PASSWORD})
    assert other.get("/clients").status_code == 200
    r = browser.post("/account/password", {"current": PASSWORD, "new": "another-pass-2", "new2": "another-pass-2"})
    assert r.status_code == 303
    assert other.get("/clients", follow_redirects=False).status_code == 303
    assert browser.get("/clients").status_code == 200
    assert rows(ctx, User)[0].session_epoch == 1


# --- страницы ------------------------------------------------------------------

def test_every_registered_page_renders(browser, ctx):
    for page in ctx.pages:
        r = browser.get(page.path)
        assert r.status_code == 200, (page.id, r.text[:500])
        assert "<html" in r.text


def test_security_headers(browser):
    r = browser.get("/")
    assert r.headers["x-frame-options"] == "DENY"
    assert "default-src 'self'" in r.headers["content-security-policy"]
    assert r.headers["cache-control"] == "no-store"


def test_404_page(browser):
    r = browser.get("/clients/999")
    assert r.status_code == 404 and "Клиент не найден" not in r.text  # общий понятный текст
    assert "Страница не найдена" in r.text


# --- клиенты и счета -------------------------------------------------------------

def make_client(browser, **data):
    payload = {"name": "Алексей", "status": "active", "telegram": "https://t.me/amorozov"} | data
    r = browser.post("/clients/new", payload)
    assert r.status_code == 303, r.text
    return int(r.headers["location"].rsplit("/", 1)[1])


def test_client_create_validates_and_normalizes(browser, ctx):
    r = browser.post("/clients/new", {"name": "", "email": "bad", "status": "active"})
    assert r.status_code == 422
    cid = make_client(browser)
    client = rows(ctx, Client)[0]
    assert client.id == cid and client.telegram == "amorozov"
    page = browser.get(f"/clients/{cid}").text
    assert "@amorozov" in page


def test_invoice_paid_extends_paid_until(browser, ctx):
    cid = make_client(browser, status="lead")
    r = browser.post(f"/clients/{cid}/invoices", {"title": "Аудит", "amount": "450", "currency": "USD",
                                                    "covers_until": iso(30)})
    assert r.status_code == 303
    invoice = rows(ctx, Invoice)[0]
    assert invoice.number == f"{date.today().year}-001" and invoice.status == "issued"
    browser.post(f"/invoices/{invoice.id}/status", {"to": "paid"})
    client = rows(ctx, Client)[0]
    assert client.paid_until == date.today() + timedelta(days=30)
    assert client.status == "active"  # первый оплаченный счёт делает лид клиентом
    # недопустимый переход: оплаченный счёт нельзя «отменить»
    browser.post(f"/invoices/{invoice.id}/status", {"to": "cancelled"})
    assert rows(ctx, Invoice)[0].status == "paid"


def test_invoice_numbers_are_unique(browser, ctx):
    cid = make_client(browser)
    for _ in range(3):
        browser.post(f"/clients/{cid}/invoices", {"title": "x", "amount": "1", "currency": "USD"})
    numbers = [i.number for i in rows(ctx, Invoice)]
    assert len(set(numbers)) == 3
    r = browser.post(f"/clients/{cid}/invoices", {"title": "x", "amount": "1", "currency": "USD", "number": numbers[0]})
    assert len(rows(ctx, Invoice)) == 3 and r.status_code == 303


def test_client_with_targets_cannot_be_deleted(browser, ctx):
    cid = make_client(browser)
    browser.post("/targets/new", {"domain": "example.com", "client_id": str(cid)})
    browser.post(f"/clients/{cid}/delete")
    assert len(rows(ctx, Client)) == 1
    tid = rows(ctx, Target)[0].id
    browser.post(f"/targets/{tid}/delete")
    browser.post(f"/clients/{cid}/delete")
    assert rows(ctx, Client) == []


# --- цели ----------------------------------------------------------------------

def test_target_normalization_and_duplicates(browser, ctx):
    r = browser.post("/targets/new", {"domain": "https://WWW.Пример.рф/shop", "client_id": ""})
    assert r.status_code == 303
    target = rows(ctx, Target)[0]
    assert target.domain == "xn--e1afmkfd.xn--p1ai" and target.name == "пример.рф"
    r = browser.post("/targets/new", {"domain": "пример.рф", "client_id": ""})
    assert r.status_code == 422 and "уже есть" in r.text
    r = browser.post("/targets/new", {"domain": "example.org", "include": "not a host", "client_id": ""})
    assert r.status_code == 422 and "Не разобрал" in r.text


def test_authorization_lifecycle(browser, ctx):
    browser.post("/targets/new", {"domain": "example.com", "include": "203.0.113.10", "exclude": "pay.example.com"})
    t = rows(ctx, Target)[0]
    assert t.auth_state == "pending" and not t.is_authorized
    # без подтверждения разрешение не ставится
    browser.post(f"/targets/{t.id}/authorize", {"method": "document", "note": ""})
    assert rows(ctx, Target)[0].auth_status == "pending"
    browser.post(f"/targets/{t.id}/authorize", {"method": "document", "note": "Договор №1", "valid_until": iso(10)})
    t = rows(ctx, Target)[0]
    assert t.is_authorized and t.auth_note == "Договор №1"
    # смена домена сбрасывает разрешение
    token = t.auth_token
    browser.post(f"/targets/{t.id}/edit", {"domain": "example.net", "client_id": ""})
    t = rows(ctx, Target)[0]
    assert t.auth_status == "pending" and t.auth_token != token
    browser.post(f"/targets/{t.id}/authorize", {"method": "dns_txt", "note": "TXT найден"})
    browser.post(f"/targets/{t.id}/revoke")
    assert rows(ctx, Target)[0].auth_state == "revoked"
    actions = [e.action for e in rows(ctx, AuditEntry)]
    assert "target.authorized" in actions and "target.revoked" in actions


def test_expired_authorization(ctx, browser):
    browser.post("/targets/new", {"domain": "example.com"})
    with ctx.db.session() as s:
        t = s.scalars(select(Target)).one()
        t.auth_status = "verified"
        t.auth_valid_until = date.today() - timedelta(days=1)
    t = rows(ctx, Target)[0]
    assert t.auth_state == "expired" and not t.is_authorized
    assert "Разрешение истекло" in browser.get("/targets").text


def test_scope_check_endpoint(browser, ctx):
    browser.post("/targets/new", {"domain": "example.com", "exclude": "pay.example.com"})
    tid = rows(ctx, Target)[0].id
    ok = browser.client.get(f"/targets/{tid}/scope", params={"host": "api.example.com"}).json()
    assert ok["allowed"] is True
    bad = browser.client.get(f"/targets/{tid}/scope", params={"host": "pay.example.com"}).json()
    assert bad["allowed"] is False and "Исключено" in bad["reason"]


# --- настройки, поиск, тема ------------------------------------------------------

def test_settings_page_saves_and_audits(browser, ctx):
    r = browser.post("/settings/general", {"brand_name": "Моя панель", "operator_name": "", "timezone": "UTC",
                                           "currency": "EUR", "remind_days": "7"})
    assert r.status_code == 303
    assert ctx.settings.get("general.brand_name") == "Моя панель"
    assert "Моя панель" in browser.get("/").text
    assert any(e.action == "settings.updated" for e in rows(ctx, AuditEntry))
    r = browser.post("/settings/general", {"brand_name": "X", "timezone": "UTC", "currency": "EUR", "remind_days": "0"})
    assert r.status_code == 422
    assert ctx.settings.get("general.brand_name") == "Моя панель"


def test_theme_api(browser, ctx):
    r = browser.json_post("/api/theme", {"theme": "light"})
    assert r.status_code == 200 and ctx.settings.get("appearance.theme") == "light"
    assert browser.json_post("/api/theme", {"theme": "neon"}).status_code == 400
    assert browser.client.post("/api/theme", json={"theme": "dark"}).status_code == 403


def test_search(browser):
    make_client(browser, name="Ирина Соколова", company="Ромашка")
    browser.post("/targets/new", {"domain": "romashka.example"})
    items = browser.client.get("/api/search", params={"q": "ромаш"}).json()["items"]
    kinds = {i["kind"] for i in items}
    assert "Клиенты" in kinds
    items = browser.client.get("/api/search", params={"q": "romashka"}).json()["items"]
    assert any(i["kind"] == "Цели" for i in items)
    assert browser.client.get("/api/search").json()["items"]  # пустой запрос — список разделов
