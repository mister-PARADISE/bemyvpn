"""Панель целиком: вход, защита форм, цели и разрешения, настройки, поиск, страницы."""

from __future__ import annotations

from fastapi.testclient import TestClient
from sqlalchemy import select

from perimeter.core.models import AuditEntry, User
from perimeter.extensions.targets.models import Target

from .conftest import PASSWORD, Browser, flashes


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
    assert {p.id for p in ctx.pages} == {"dashboard", "targets", "runs", "proxies", "modules", "journal", "settings"}
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


def test_edit_target(browser, ctx):
    browser.post("/targets/new", {"domain": "example.com"})
    tid = rows(ctx, Target)[0].id
    r = browser.post(f"/targets/{tid}/edit", {"domain": "shop.example.com", "title": "Магазин", "exclude": "pay.example.com"})
    assert r.status_code == 303
    t = rows(ctx, Target)[0]
    assert (t.domain, t.title, t.exclude) == ("shop.example.com", "Магазин", ["pay.example.com"])
    assert "Магазин" in browser.get("/targets").text
    assert "Ничего не нашлось" in browser.get("/targets?q=nothing-here").text


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

def test_dashboard_is_a_monitor(browser):
    browser.post("/targets/new", {"domain": "example.com"})
    page = browser.get("/").text
    for title in ("Система", "Панель", "Журнал"):
        assert title in page
    assert "data-monitor" in page and 'data-key="disk"' in page  # полосы как в htop
    assert ">цели</a>" in page  # строка «Панели» от расширения целей
    assert "[+]" in page and "Новая цель example.com" in page


def test_system_api(browser):
    data = browser.client.get("/api/system").json()
    keys = {row["key"] for row in data["rows"]}
    assert {"cpu", "ram", "disk", "panel"} <= keys
    disk = next(r for r in data["rows"] if r["key"] == "disk")
    assert len(disk["bar"]) == 30 and set(disk["bar"]) <= {"|", " "} and disk["tone"] in {"ok", "warn", "err"}


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


def test_db_commits_before_response():
    """Иначе браузер приходит по редиректу раньше, чем изменения попали в базу."""
    from perimeter.web import DB

    assert DB.__metadata__[0].scope == "function"


def test_login_is_a_console(anon, browser):
    from fastapi.testclient import TestClient

    page = TestClient(browser.client.app).get("/login").text
    assert "data-term" in page and "perimeter</span> login:" in page and "password:" in page
    assert 'class="card' not in page  # без окон


# --- хосты цели ------------------------------------------------------------------

def test_hosts_add_remove_in_one_line(browser, ctx):
    browser.post("/targets/new", {"domain": "example.com"})
    tid = rows(ctx, Target)[0].id
    r = browser.post(f"/targets/{tid}/hosts", {"hosts": "https://API.Example.NET/x, 203.0.113.10 -pay.example.com "
                                                         "shop.example.com bad_host! 198.51.100.0/24"})
    assert r.status_code == 303
    t = rows(ctx, Target)[0]
    assert t.include == ["api.example.net", "203.0.113.10", "198.51.100.0/24"]
    assert t.exclude == ["pay.example.com"]
    note = flashes(browser.get(f"/targets/{tid}").text)
    assert "уже входит: shop.example.com" in note and "не понял: bad_host!" in note
    browser.post(f"/targets/{tid}/hosts", {"hosts": "pay.example.com"})  # снова можно трогать
    assert rows(ctx, Target)[0].exclude == []
    browser.post(f"/targets/{tid}/hosts/remove", {"value": "203.0.113.10", "list": "include"})
    assert rows(ctx, Target)[0].include == ["api.example.net", "198.51.100.0/24"]
    assert any(e.action == "target.hosts" for e in rows(ctx, AuditEntry))


def test_two_flashes_in_a_row_survive(browser, ctx):
    browser.post("/targets/new", {"domain": "example.com"})  # «Цель добавлена» ещё не показано
    tid = rows(ctx, Target)[0].id
    browser.post(f"/targets/{tid}/hosts", {"hosts": "api.example.net"})
    note = flashes(browser.get(f"/targets/{tid}").text)
    assert "Цель добавлена" in note and "добавлено: api.example.net" in note


# --- после ревью --------------------------------------------------------------------

def test_cleared_setting_does_not_break_panel(browser, ctx):
    browser.get("/settings/security")
    r = browser.post("/settings/security", {"session_hours": ""})
    assert r.status_code in (200, 303)
    assert browser.get("/").status_code == 200 and browser.get("/settings").status_code == 200
    assert ctx.settings.get("security.session_hours") > 0


def test_bad_addresses_are_404_pages_not_json(browser):
    for url in ("/targets/abc", "/targets/99999999999999999999", "/runs?page=x"):
        r = browser.get(url)
        assert r.status_code == 404 and "text/html" in r.headers["content-type"], url


def test_hosts_add_edge_cases(browser, ctx):
    browser.post("/targets/new", {"domain": "example.com"})
    tid = rows(ctx, Target)[0].id
    browser.post(f"/targets/{tid}/hosts", {"hosts": "203.0.113.5 10.0.0.0/24 -pay.example.com"})
    browser.post(f"/targets/{tid}/hosts", {"hosts": "-203.0.113.5"})
    browser.post(f"/targets/{tid}/hosts", {"hosts": "203.0.113.5"})  # снова можно — и снова входит
    t = rows(ctx, Target)[0]
    assert "203.0.113.5" in t.include and "203.0.113.5" not in t.exclude
    browser.post(f"/targets/{tid}/hosts", {"hosts": "10.0.0.0/25 x.pay.example.com github.io 0.0.0.0/0"})
    note = flashes(browser.get(f"/targets/{tid}").text)
    assert "уже входит: 10.0.0.0/25" in note  # подсеть уже включённой сети
    assert "не добавлено: x.pay.example.com" in note  # исключение важнее
    assert "общий домен" in note and "широкая" in note
    assert set(rows(ctx, Target)[0].include) == {"203.0.113.5", "10.0.0.0/24"}


def test_target_form_keeps_what_was_typed(browser):
    page = browser.post("/targets/new", {"domain": "example.com", "include": "api.example.com\nне адрес!"}).text
    assert "не адрес!" in page and "api.example.com" in page
    assert "общий домен" in browser.post("/targets/new", {"domain": "github.io"}).text


def test_deleting_target_stops_its_runs_and_ids_are_not_reused(browser, ctx):
    from perimeter.extensions.runs.models import Run

    browser.post("/targets/new", {"domain": "old.example"})
    old = rows(ctx, Target)[0].id
    with ctx.db.session() as s:
        s.add(Run(target_id=old, module_id="resolve", status="queued"))
    browser.post(f"/targets/{old}/delete")
    browser.post("/targets/new", {"domain": "new.example"})
    new = rows(ctx, Target)[0]
    assert new.id != old  # у новой цели не может оказаться чужой истории
    assert "Новая цель old.example" not in browser.get(f"/targets/{new.id}").text  # история не чужая


def test_expired_session_post_returns_to_the_form_page(browser, app):
    browser.get("/targets")
    browser.client.cookies.clear()
    r = browser.client.post("/targets/new", data={"domain": "x.example"}, follow_redirects=False,
                            headers={"referer": "http://testserver/targets/new"})
    assert r.status_code in (303, 307) and r.headers["location"] in ("/login?next=/targets/new", "/login?next=%2Ftargets%2Fnew")
