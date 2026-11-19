"""Данные модулей, которые панель обновляет сама: расписание, ошибки, повтор, свой источник из расширения."""

from __future__ import annotations

from datetime import timedelta

from sqlalchemy import select

from perimeter.core.db import utcnow
from perimeter.core.feeds import Feed
from perimeter.extensions.feeds.models import FeedState
from perimeter.extensions.feeds.scheduler import NEVER, RETRY

from .conftest import make_app
from .test_extensions import login, write_plugin

PLUGIN = '''
from perimeter.core.feeds import Feed

CALLS = []

async def refresh(job):
    CALLS.append(job.setting("feeds.auto"))
    job.log("[*] качаю список")
    if len(CALLS) == 2:
        raise RuntimeError("источник лежит")
    job.items, job.changed = 3, 1
    return "записей 3"

def setup(ctx):
    ctx.feeds.add(Feed("demo.list", "Список адресов", refresh, source="example.org", every=12, modules=("resolve",),
                       description="Свой источник расширения"))
'''


def test_schedule_math(tmp_path):
    app = make_app(tmp_path)
    ctx = app.state.ctx
    sched = ctx.services["feeds"]
    feed = Feed("x.y", "Икс", lambda job: None, every=24)
    assert sched.next_at(feed, None) == NEVER  # ни разу не обновлялись — пора
    now = utcnow()
    ok = FeedState(id="x.y", status="ok", finished_at=now - timedelta(hours=1), ok_at=now - timedelta(hours=1))
    assert sched.next_at(feed, ok) == ok.ok_at + timedelta(hours=24)
    assert not sched.due(feed, ok, now)
    failed = FeedState(id="x.y", status="failed", finished_at=now, ok_at=now - timedelta(days=3))
    assert sched.next_at(feed, failed) == now + RETRY  # не получилось — пробуем снова через полчаса, а не через сутки
    assert sched.next_at(Feed("z", "Зет", lambda job: None, every=0), None) is None  # только кнопкой
    assert sched.next_at(Feed("s", "Эс", lambda job: None, every=lambda c: c.settings.get("vulndb.cache_hours")), ok) \
        == ok.ok_at + timedelta(hours=24)  # расписание из настроек
    ctx.settings.set("feeds.auto", False)
    assert sched.next_at(feed, None) is None  # сами не обновляются — выключено в настройках


def test_extension_feed_runs_fails_and_is_listed(tmp_path):
    write_plugin(tmp_path, "lists", PLUGIN)
    app = make_app(tmp_path)
    ctx = app.state.ctx
    assert "demo.list" in ctx.feeds
    sched, engine = ctx.services["feeds"], ctx.services["engine"]
    try:
        first = engine.spawn(lambda: sched.cycle(only=["demo.list"], actor="admin")).result(timeout=10)
        assert first["feeds"] == 1 and first["changes"] == ["Список адресов +1"]
        with ctx.db.session() as s:
            st = s.get(FeedState, "demo.list")
            assert (st.status, st.items, st.changed, st.summary, st.actor) == ("ok", 3, 1, "записей 3", "admin")
            assert any("качаю список" in line[2] for line in st.log)
        # «Адреса» — активный? нет, пассивный, но целей нет — перепроверять нечего
        engine.spawn(lambda: sched.cycle(only=["demo.list"])).result(timeout=10)
        with ctx.db.session() as s:
            st = s.get(FeedState, "demo.list")
            assert st.status == "failed" and "источник лежит" in st.error and st.items == 3  # прошлые данные остались
        b = login(app)
        page = b.get("/modules?tab=data").text
        assert "Список адресов" in page and "example.org" in page and "источник лежит" in page
        assert "раз в 12 ч" in page and "Пользуются: Адреса" in page
        assert "Данные: Список адресов" in b.get("/modules").text  # у модуля в каталоге видно, на чём он живёт
        assert "данные модулей" in b.get("/").text and "ошибка: Список адресов" in b.get("/").text
        assert "источник лежит" in b.get("/journal").text
        with ctx.db.session() as s:
            assert [r.id for r in s.scalars(select(FeedState))] == ["demo.list"]
    finally:
        ctx.call_hooks("app.stop", ctx)


def test_broken_extension_feed_is_rolled_back(tmp_path):
    write_plugin(tmp_path, "bad", '''
from perimeter.core.feeds import Feed
async def refresh(job):
    return ""
def setup(ctx):
    ctx.feeds.add(Feed("bad.feed", "Плохое", refresh))
    raise RuntimeError("упало")
''')
    ctx = make_app(tmp_path).state.ctx
    assert "bad.feed" not in ctx.feeds  # откат: источник сломанного расширения не остался
