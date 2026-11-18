"""Движок запусков: отмена, падение, таймаут, перезапуск панели, проверка адресов цели."""

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

from .conftest import PASSWORD, Browser, make_app, run_and_wait, wait_run

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

def setup(ctx):
    for m in (Sleepy, Boom, Slow, Sneaky):
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
        s.add(Asset(target_id=tid, kind="ip", value="93.184.216.34", data={"domains": ["example.com"]}))
        s.add(Asset(target_id=tid, kind="ip", value="104.16.1.1", data={"domains": ["example.com"], "cdn": "Cloudflare"}))
        s.add(Asset(target_id=tid, kind="neighbor", value="93.184.216.40", data={"ptr": "x"}))
    view = TargetView(tid, "example.com", Scope.build("example.com", [], ["pay.example.com"]))
    rc = RunContext(ctx.services["engine"], 0, OpenPorts, view, {})
    assert rc.allows("api.example.com")[0]
    assert rc.allows("93.184.216.34")[0]  # адрес, в который указывает имя цели
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
