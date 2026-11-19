"""Обновление данных модулей по расписанию — в потоке движка запусков.

В том же цикле asyncio, что и модули: общие ограничения (например, пауза между запросами к NVD)
действуют на всех сразу, а модуль, которому данные нужны прямо сейчас, и фоновое обновление
не качают одно и то же дважды.
"""

from __future__ import annotations

import asyncio
import logging
import time
from datetime import datetime, timedelta
from typing import Any

from sqlalchemy import func, select

from ...core.context import Context
from ...core.db import utcnow
from ...core.feeds import Feed
from ...core.modules import Kind
from ..runs.models import ACTIVE_STATES, Run
from .models import FeedState

log = logging.getLogger("perimeter.feeds")

SYSTEM = "расписание"  # кто запустил: так подписаны фоновые обновления и перепроверки в журнале и запусках
RETRY = timedelta(minutes=30)
NEVER = datetime(1970, 1, 1)  # ещё ни разу не обновлялись — пора  # не получилось — когда пробовать снова (но не реже, чем по расписанию)
LOG_LINES = 300


class Job:
    """FeedJob для refresh: консоль обновления, настройки, папка данных."""

    def __init__(self, ctx: Context, feed: Feed) -> None:
        self.ctx = ctx
        self.feed = feed
        self.changed = 0
        self.items = 0
        self.lines: list[list[str]] = []
        self._flushed = 0.0

    @property
    def data_dir(self):
        return self.ctx.config.data_dir

    def setting(self, key: str) -> Any:
        return self.ctx.settings.get(key)

    def log(self, text: str, level: str = "info") -> None:
        self.lines.append([utcnow().isoformat(timespec="seconds"), level, str(text)[:1000]])
        if time.monotonic() - self._flushed > 2:  # страница «Данные» видит ход обновления
            self.flush()

    def flush(self) -> None:
        self._flushed = time.monotonic()
        try:
            with self.ctx.db.session() as s:
                state = s.get(FeedState, self.feed.id)
                if state is not None:
                    state.log = self.lines[-LOG_LINES:]
        except Exception:  # noqa: BLE001 — строка консоли не повод уронить обновление
            log.warning("Не записался ход обновления %s", self.feed.id, exc_info=True)


class Scheduler:
    TICK = 60  # как часто смотреть, не пора ли что-то обновить, с
    FIRST = 20  # первая проверка — чуть позже старта, чтобы не мешать запуску панели

    def __init__(self, ctx: Context) -> None:
        self.ctx = ctx
        self.running: set[str] = set()

    # --- когда ------------------------------------------------------------------
    def next_at(self, feed: Feed, state: FeedState | None) -> datetime | None:
        """Когда обновить в следующий раз; None — сама не обновляет (выключено или только кнопкой)."""
        hours = feed.hours(self.ctx)
        if hours <= 0 or not self.ctx.settings.get("feeds.auto"):
            return None
        if state is None or state.finished_at is None:
            return NEVER
        if state.status == "failed":
            gap = min(RETRY, timedelta(hours=hours))
            return state.finished_at + gap
        return (state.ok_at or state.finished_at) + timedelta(hours=hours)

    def due(self, feed: Feed, state: FeedState | None, now: datetime) -> bool:
        if feed.id in self.running:
            return False
        when = self.next_at(feed, state)
        return when is not None and when <= now

    def states(self) -> dict[str, FeedState]:
        with self.ctx.db.session() as s:
            rows = s.scalars(select(FeedState)).all()
            s.expunge_all()
            return {r.id: r for r in rows}

    # --- цикл -------------------------------------------------------------------
    def start(self) -> None:
        if self.ctx.config.background and len(self.ctx.feeds):
            self.ctx.services["engine"].spawn(self.forever)

    async def forever(self) -> None:
        await asyncio.sleep(self.FIRST)
        while True:
            try:
                await self.cycle()
            except asyncio.CancelledError:
                raise
            except Exception:  # noqa: BLE001 — расписание не должно умереть от одной ошибки
                log.exception("Обновление данных модулей упало")
            await asyncio.sleep(self.TICK)

    async def cycle(self, only: list[str] | None = None, actor: str = SYSTEM) -> dict[str, Any]:
        """Обновить то, чему пора (или только перечисленное), потом перепроверить цели."""
        started = utcnow()
        states = self.states()

        def wanted(feed: Feed) -> bool:
            if only is not None:  # кнопка «обновить сейчас»: не ждём расписания
                return feed.id in only and feed.id not in self.running
            return self.due(feed, states.get(feed.id), started)

        todo = [feed for feed in self.ctx.feeds if wanted(feed)]
        if not todo:
            return {"feeds": 0, "rechecked": 0}
        modules: set[str] = set()
        changes: list[str] = []
        for feed in todo:
            job = await self.run_feed(feed, actor)
            if job is not None and job.changed:
                modules.update(feed.modules)
                changes.append(f"{feed.title} {job.changed:+d}")
        rechecked = 0
        if modules and self.ctx.settings.get("feeds.recheck"):
            rechecked = self.recheck(sorted(modules), started)
            with self.ctx.db.session() as s:
                for feed in todo:
                    state = s.get(FeedState, feed.id)
                    if state is not None and set(feed.modules) & modules:
                        state.rechecked = rechecked
        if changes or rechecked:
            with self.ctx.db.session() as s:
                self.ctx.audit(s, "feeds.updated", "Данные модулей обновлены: " + ", ".join(changes)
                               + (f"; перепроверяю целей: {rechecked}" if rechecked else ""), actor=actor)
        return {"feeds": len(todo), "rechecked": rechecked, "changes": changes}

    async def run_feed(self, feed: Feed, actor: str = SYSTEM) -> Job | None:
        if feed.id in self.running:
            return None
        self.running.add(feed.id)
        job = Job(self.ctx, feed)
        with self.ctx.db.session() as s:
            state = s.get(FeedState, feed.id)
            if state is None:
                state = FeedState(id=feed.id)
                s.add(state)
            state.status, state.started_at, state.actor, state.log = "running", utcnow(), actor, []
        status, summary, error = "ok", "", ""
        started = time.monotonic()
        try:
            job.log(f"$ perimeter update {feed.id}", "dim")
            summary = await asyncio.wait_for(feed.refresh(job), timeout=feed.timeout) or ""
        except asyncio.CancelledError:
            status, error = "failed", "остановлено: панель выключили"
            raise
        except TimeoutError:
            status, error = "failed", f"не уложились в {feed.timeout} с"
        except Exception as exc:  # noqa: BLE001 — ошибка источника не роняет расписание
            log.warning("Обновление %s не удалось", feed.id, exc_info=True)
            status, error = "failed", f"{type(exc).__name__}: {exc}"[:500]
        finally:
            self.running.discard(feed.id)
            seconds = time.monotonic() - started
            if status == "ok":
                job.log(f"[done] {summary or 'готово'}  за {seconds:.1f} с", "ok")
            else:
                job.log(f"[err] {error}", "err")
            self._save(feed, job, status, summary, error, actor)
        return job

    def _save(self, feed: Feed, job: Job, status: str, summary: str, error: str, actor: str) -> None:
        with self.ctx.db.session() as s:
            state = s.get(FeedState, feed.id) or FeedState(id=feed.id)
            s.add(state)
            now = utcnow()
            state.status, state.finished_at, state.error = status, now, error
            state.log = job.lines[-LOG_LINES:]
            state.count = (state.count or 0) + 1
            if status == "ok":
                state.ok_at, state.summary, state.changed, state.items = now, summary, job.changed, job.items
            if status == "failed" or actor != SYSTEM:  # плановые удачные — одной строкой за цикл, не по каждому
                text = error if status == "failed" else summary
                self.ctx.audit(s, f"feed.{status}", f"Данные «{feed.title}»: {text}", actor=actor,
                               data={"feed": feed.id})

    # --- после обновления -----------------------------------------------------------
    def recheck(self, module_ids: list[str], since: datetime) -> int:
        """Пассивные модули перепроверяют цели, где уже работали и с тех пор не запускались.
        К сайтам они не обращаются, поэтому это можно делать без спроса. Активные — никогда."""
        queued: list[int] = []
        with self.ctx.db.session() as s:
            for mid in module_ids:
                module = self.ctx.modules.get(mid)
                if module is None or module.spec.kind is not Kind.PASSIVE:
                    continue
                last = dict(s.execute(select(Run.target_id, func.max(Run.finished_at))
                                      .where(Run.module_id == mid, Run.status == "done")
                                      .group_by(Run.target_id)).all())
                busy = set(s.scalars(select(Run.target_id).where(Run.module_id == mid,
                                                                 Run.status.in_(ACTIVE_STATES))))
                for target_id, finished in last.items():
                    if target_id in busy or finished is None or finished >= since:
                        continue
                    previous = s.scalar(select(Run).where(Run.target_id == target_id, Run.module_id == mid,
                                                          Run.status == "done").order_by(Run.id.desc()).limit(1))
                    run = Run(target_id=target_id, module_id=mid, params=dict(previous.params or {}) if previous else {},
                              actor=SYSTEM)
                    s.add(run)
                    s.flush()
                    queued.append(run.id)
        engine = self.ctx.services["engine"]
        for run_id in queued:  # по одному: неудача на одной цели не останавливает другие
            engine.submit([run_id])
        return len(queued)
