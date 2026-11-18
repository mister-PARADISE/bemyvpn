"""Движок запусков: зависимости и цепочки, отмена, падение, таймаут, перезапуск панели, проверка адресов цели."""

from __future__ import annotations

import asyncio
import time
from textwrap import dedent

import pytest
from fastapi.testclient import TestClient
from sqlalchemy import select

from perimeter.core.modules import OutOfScope
from perimeter.core.scope import Scope
from perimeter.extensions.recon.modules import Neighbors, OpenPorts
from perimeter.extensions.runs.engine import RunContext, TargetView
from perimeter.extensions.runs.models import Asset, Run, RunLine

from .conftest import PASSWORD, Browser, make_app, run_and_wait, wait_chain, wait_run

PLUGIN = '''
import asyncio
from perimeter.core import fields as f
from perimeter.core.modules import Kind, Module, ModuleSpec

class Sleepy(Module):
    spec = ModuleSpec(id="t_sleepy", title="Сонный", kind=Kind.PASSIVE, consumes=("domain",),
                      params=(f.integer("seconds", "Сколько", default=10),))
    async def run(self, run):
        run.log("засыпаю")
        await run.emit("dns_record", "TXT hello", type="TXT")
        for i in range(run.params["seconds"] * 10):
            run.progress(i, run.params["seconds"] * 10, "тиков")
            await asyncio.sleep(0.1)

class Boom(Module):
    spec = ModuleSpec(id="t_boom", title="Ломается", kind=Kind.PASSIVE, consumes=("domain",))
    async def run(self, run):
        raise RuntimeError("всё сломалось")

class Slow(Module):
    spec = ModuleSpec(id="t_slow", title="Медленный", kind=Kind.PASSIVE, consumes=("domain",), timeout=1)
    async def run(self, run):
        await asyncio.sleep(5)

class Sneaky(Module):
    spec = ModuleSpec(id="t_sneaky", title="Лезет не туда", kind=Kind.ACTIVE, consumes=("domain",))
    async def run(self, run):
        await run.connect("evil.test", 80)

class Base(Module):
    spec = ModuleSpec(id="t_base", title="Основа", kind=Kind.PASSIVE, consumes=("domain",), produces=("dns_record",))
    FAIL = False
    async def run(self, run):
        if type(self).FAIL:
            raise RuntimeError("основа сломалась")
        await run.emit("dns_record", "TXT base", type="TXT")

class Child(Module):
    spec = ModuleSpec(id="t_child", title="Надстройка", kind=Kind.PASSIVE, consumes=("domain",), needs=("t_base",))
    async def run(self, run):
        run.log("надстройка работает")

class Grand(Module):
    spec = ModuleSpec(id="t_grand", title="Над надстройкой", kind=Kind.PASSIVE, consumes=("domain",), needs=("t_child",))
    async def run(self, run):
        run.log("и я работаю")

class After(Module):
    spec = ModuleSpec(id="t_after", title="После сонного", kind=Kind.PASSIVE, consumes=("domain",), needs=("t_sleepy",))
    async def run(self, run):
        run.log("дождался")

def setup(ctx):
    for m in (Sleepy, Boom, Slow, Sneaky, Base, Child, Grand, After):
        ctx.modules.add(m)
'''


@pytest.fixture
def lab(tmp_path):
    plug = tmp_path / "data" / "plugins" / "testmods"
    plug.mkdir(parents=True)
    (plug / "__init__.py").write_text(dedent(PLUGIN), encoding="utf-8")
    app = make_app(tmp_path)
    b = Browser(TestClient(app))
    b.get("/setup")
    b.post("/setup", {"username": "admin", "password": PASSWORD, "password2": PASSWORD})
    b.get("/")
    r = b.post("/targets/new", {"domain": "example.com"})
    tid = int(r.headers["location"].rsplit("/", 1)[1])
    yield app, b, tid
    app.state.ctx.call_hooks("app.stop", app.state.ctx)


def test_run_live_output_and_cancel(lab):
    app, b, tid = lab
    r = b.post(f"/targets/{tid}/run", {"module": "t_sleepy"})
    run_id = int(r.headers["location"].split("run=")[1].split("#")[0])
    deadline = time.monotonic() + 10
    while True:
        state = b.client.get(f"/runs/{run_id}/log").json()
        if state["status"] == "running" and any("засыпаю" in ln["text"] for ln in state["lines"]):
            break
        assert time.monotonic() < deadline
        time.sleep(0.05)
    # консоль на странице цели видна и умеет догружать вывод с места
    page = b.get(f"/targets/{tid}?run={run_id}").text
    assert 'data-console="%d"' % run_id in page and 'data-active="1"' in page
    later = b.client.get(f"/runs/{run_id}/log", params={"after": state["lines"][-1]["n"]}).json()
    assert all(ln["n"] > state["lines"][-1]["n"] for ln in later["lines"])
    b.post(f"/runs/{run_id}/cancel", {"back": f"/targets/{tid}"})
    final = wait_run(b, run_id, 10)
    assert final["status"] == "cancelled" and "[stop]" in final["text"]
    assert "найдено 1" in final["summary"]  # находки до остановки сохранились


def test_failure_and_timeout_are_reported(lab):
    _app, b, tid = lab
    boom = run_and_wait(b, tid, "t_boom")
    assert boom["status"] == "failed" and boom["error"] == "RuntimeError: всё сломалось"
    slow = run_and_wait(b, tid, "t_slow")
    assert slow["status"] == "failed" and "превышено время" in slow["error"]
    journal = b.get("/journal").text
    assert "всё сломалось" in journal and "[x]" in journal  # провал в журнале — красным крестом


def test_active_module_cannot_leave_target(lab):
    _app, b, tid = lab
    state = run_and_wait(b, tid, "t_sneaky")
    assert state["status"] == "failed" and "не входит в цель" in state["error"] and "evil.test" in state["error"]


def test_queued_run_cancel_and_again(lab):
    app, b, tid = lab
    ctx = app.state.ctx
    with ctx.db.session() as s:
        run = Run(target_id=tid, module_id="t_boom")
        s.add(run)
        s.flush()
        run_id = run.id
    b.post(f"/runs/{run_id}/cancel")
    with ctx.db.session() as s:
        assert s.get(Run, run_id).status == "cancelled"
    r = b.post(f"/runs/{run_id}/again")
    new_id = int(r.headers["location"].split("run=")[1].split("#")[0])
    assert wait_run(b, new_id)["status"] == "failed"
    assert "Запуск #%d" % new_id in b.get(f"/runs/{new_id}").text
    assert "Ломается" in b.get("/runs").text


def test_restart_marks_interrupted(tmp_path):
    app = make_app(tmp_path)
    ctx = app.state.ctx
    with ctx.db.session() as s:
        from perimeter.extensions.targets.models import Target
        t = Target(domain="example.com")
        s.add(t)
        s.flush()
        s.add(Run(target_id=t.id, module_id="hostinfo", status="running"))
    ctx2 = make_app(tmp_path).state.ctx  # панель перезапустили
    with ctx2.db.session() as s:
        run = s.scalars(select(Run)).one()
        assert run.status == "interrupted"
        assert "панель перезапустили" in s.scalars(select(RunLine)).one().text


def test_scope_rules_for_active_modules(app):
    ctx = app.state.ctx
    with ctx.db.session() as s:
        from perimeter.extensions.targets.models import Target
        t = Target(domain="example.com", exclude=["pay.example.com"])
        s.add(t)
        s.flush()
        tid = t.id
        s.add(Asset(target_id=tid, kind="ip", value="93.184.216.34", data={"domains": ["example.com"], "checked": True}))
        s.add(Asset(target_id=tid, kind="ip", value="104.16.1.1", data={"domains": ["example.com"], "cdn": "Cloudflare"}))
        s.add(Asset(target_id=tid, kind="ip", value="198.51.100.7", data={"domains": ["www.example.com"]}))
        s.add(Asset(target_id=tid, kind="neighbor", value="93.184.216.40", data={"ptr": "x"}))
    view = TargetView(tid, "example.com", Scope.build("example.com", [], ["pay.example.com"]))
    rc = RunContext(ctx.services["engine"], 0, OpenPorts, view, {})
    assert rc.allows("api.example.com")[0]
    assert rc.allows("93.184.216.34")[0]  # адрес, в который указывает имя цели
    # провайдер ещё не проверен — вдруг CDN: только как на сайт
    assert rc.allows("198.51.100.7", 443)[0] and not rc.allows("198.51.100.7", 22)[0]
    assert "Провайдер" in rc.allows("198.51.100.7", 22)[1]
    assert not rc.allows("93.184.216.40")[0]  # сосед — не цель
    assert rc.allows("104.16.1.1", 443)[0] and not rc.allows("104.16.1.1", 22)[0]  # CDN: только как сайт
    assert not rc.allows("104.16.1.1")[0]
    ok, reason = rc.allows("pay.example.com")
    assert not ok and "Исключено" in reason
    with pytest.raises(OutOfScope):
        asyncio.run(rc.connect("8.8.8.8", 53, 1))
    passive = RunContext(ctx.services["engine"], 0, Neighbors, view, {})
    with pytest.raises(OutOfScope, match="пассивный"):
        asyncio.run(passive.connect("93.184.216.34", 80, 1))


# --- зависимости и цепочки ------------------------------------------------------------

def test_needs_run_first_and_fresh_results_are_reused(lab):
    _app, b, tid = lab
    state = run_and_wait(b, tid, "t_grand")
    assert state["chain"] == [("t_base", "done"), ("t_child", "done"), ("t_grand", "done")]
    assert run_and_wait(b, tid, "t_grand")["chain"] == [("t_grand", "done")]  # всё свежее — только сам
    run_and_wait(b, tid, "t_base")  # основу обновили — надстройка над ней устарела, как в make
    assert run_and_wait(b, tid, "t_grand")["chain"] == [("t_child", "done"), ("t_grand", "done")]
    b.post(f"/targets/{tid}/assets/clear")  # находки стёрли — основа снова нужна
    assert run_and_wait(b, tid, "t_grand")["chain"] == [("t_base", "done"), ("t_child", "done"), ("t_grand", "done")]


def test_failed_need_skips_everything_built_on_it(lab, monkeypatch):
    app, b, tid = lab
    monkeypatch.setattr(app.state.ctx.modules.get("t_base"), "FAIL", True)
    state = run_and_wait(b, tid, "t_grand")
    assert state["chain"] == [("t_base", "failed"), ("t_child", "skipped"), ("t_grand", "skipped")]
    assert state["error"] == "не отработал «Надстройка»" and "[skip]" in state["text"]
    with app.state.ctx.db.session() as s:
        first = s.scalars(select(Run.id).where(Run.target_id == tid).order_by(Run.id)).first()
    page = b.get(f"/targets/{tid}?run={first}").text
    assert "── [1/3] Основа" in page and "── [3/3] Над надстройкой" in page  # вся цепочка в одной консоли
    assert "есть ошибки" in page
    assert "[-]" in b.get("/journal").text


def test_stop_all_cancels_the_rest_of_chain_and_again_repeats_it(lab):
    _app, b, tid = lab
    r = b.post(f"/targets/{tid}/run", {"module": "t_after"})
    first = int(r.headers["location"].split("run=")[1].split("#")[0])
    deadline = time.monotonic() + 10
    while b.client.get(f"/runs/{first}/log").json()["status"] != "running":
        assert time.monotonic() < deadline
        time.sleep(0.05)
    page = b.get(f"/targets/{tid}?run={first}").text
    assert "Остановить всё" in page and "── [1/2] Сонный" in page
    b.post(f"/runs/{first}/cancel")
    assert wait_chain(b, first, timeout=10)["chain"] == [("t_sleepy", "cancelled"), ("t_after", "cancelled")]
    r = b.post(f"/runs/{first}/again")
    again = int(r.headers["location"].split("run=")[1].split("#")[0])
    state = b.client.get(f"/runs/{again}/log").json()
    assert state["module"] == "t_sleepy" and state["total"] == 2 and state["next"]["module_title"] == "После сонного"
    b.post(f"/runs/{again}/cancel")
    wait_chain(b, again, timeout=10)


def test_runs_migration_adds_chain_column(tmp_path):
    app = make_app(tmp_path)
    app.state.ctx.call_hooks("app.stop", app.state.ctx)
    with app.state.ctx.db.engine.begin() as conn:  # как будто база из прошлой версии
        conn.exec_driver_sql("DROP INDEX ix_run_chain")
        conn.exec_driver_sql("ALTER TABLE run DROP COLUMN chain")
        conn.exec_driver_sql("UPDATE schema_version SET version = 1 WHERE component = 'runs'")
    app2 = make_app(tmp_path)
    with app2.state.ctx.db.engine.begin() as conn:
        assert "chain" in {row[1] for row in conn.exec_driver_sql("PRAGMA table_info(run)")}
    app2.state.ctx.call_hooks("app.stop", app2.state.ctx)


# --- после ревью --------------------------------------------------------------------

def test_ip_of_excluded_name_is_not_a_target(app):
    """Адрес остался в находках от «pay.example.com», а его потом исключили — трогать нельзя."""
    ctx = app.state.ctx
    with ctx.db.session() as s:
        from perimeter.extensions.targets.models import Target
        t = Target(domain="example.com", exclude=["pay.example.com"])
        s.add(t)
        s.flush()
        tid = t.id
        s.add(Asset(target_id=tid, kind="ip", value="203.0.113.50", data={"domains": ["pay.example.com"], "checked": True}))
    view = TargetView(tid, "example.com", Scope.build("example.com", [], ["pay.example.com"]))
    rc = RunContext(ctx.services["engine"], 0, OpenPorts, view, {})
    assert not rc.allows("203.0.113.50", 443)[0] and not rc.allows("203.0.113.50", 22)[0]


def test_long_output_shows_the_tail_with_a_marker(lab):
    app, b, tid = lab
    ctx = app.state.ctx
    with ctx.db.session() as s:
        run = Run(target_id=tid, module_id="t_boom", status="done")
        s.add(run)
        s.flush()
        s.add_all(RunLine(run_id=run.id, n=i, level="info", text=f"строка {i}") for i in range(1, 1251))
        run_id = run.id
    state = b.client.get(f"/runs/{run_id}/log").json()
    assert state["lines"][0]["text"].startswith("… скрыто строк: 250") and state["lines"][-1]["text"] == "строка 1250"
    assert len(state["lines"]) == 1001


def test_single_run_page_acts_on_that_run_only(lab):
    app, b, tid = lab
    r = b.post(f"/targets/{tid}/run", {"module": "t_after"})
    first = int(r.headers["location"].split("run=")[1].split("#")[0])
    page = b.get(f"/runs/{first}").text
    assert 'name="only" value="1"' in page  # кнопки страницы запуска — только про него
    b.post(f"/runs/{first}/cancel", {"only": "1"})
    state = wait_chain(b, first, timeout=10)
    assert state["chain"] == [("t_sleepy", "cancelled"), ("t_after", "skipped")]  # второй пропущен, не отменён
