"""Движок запусков: свой поток со своим циклом asyncio, очередь, лимит, отмена.

Запуски живут отдельно от веб-сервера, поэтому долгий скан не тормозит панель,
а панель не обрывает скан. Модулю передаётся `RunContext` — всё, что ему
разрешено: писать в консоль, сохранять находки, ходить в сеть. Ходить в сеть
активный модуль может только к адресам, которые входят в цель, — это
проверяет `RunContext`, а не автор модуля.
"""

from __future__ import annotations

import asyncio
import concurrent.futures
import json
import logging
import socket
import threading
import time
from contextlib import suppress
from dataclasses import dataclass
from typing import Any, Callable

from sqlalchemy import select, update
from sqlalchemy.exc import OperationalError

from ...core.context import Context
from ...core.db import utcnow
from ...core.modules import Kind, Module, ModuleError, OutOfScope
from ...core.scope import Scope, ScopeError, display_host, is_ip, normalize_host
from .models import Asset, Run, RunLine

log = logging.getLogger("perimeter.runs")

LEVELS = ("info", "ok", "warn", "err", "dim", "found")


@dataclass(frozen=True)
class TargetView:
    id: int
    root: str
    scope: Scope

    @property
    def name(self) -> str:
        return display_host(self.root)

    @property
    def is_ip(self) -> bool:
        return is_ip(self.root)


def _jsonable(data: dict[str, Any]) -> dict[str, Any]:
    def norm(v: Any) -> Any:
        if isinstance(v, (set, frozenset, tuple)):
            return [norm(x) for x in v]
        return v
    return json.loads(json.dumps({k: norm(v) for k, v in data.items()}, default=str))


class RunContext:
    """То, что видит модуль во время работы."""

    def __init__(self, engine: "Engine", run_id: int, module: type[Module], target: TargetView,
                 params: dict[str, Any], created: Any = None) -> None:
        self.engine = engine
        self.created = created  # когда создан запуск: по номеру запуска его не опознать, номер могут переиспользовать
        self.ctx: Context = engine.ctx
        self.run_id = run_id
        self.spec = module.spec
        self.target = target
        self.params = params
        self.summary = ""
        self.new = 0
        self.total = 0
        self._n = 0
        self._progress_at = 0.0
        self._ips: dict[str, dict[str, Any]] | None = None

    # --- консоль ---------------------------------------------------------
    def setting(self, key: str) -> Any:
        return self.ctx.settings.get(key)

    def _check_alive(self, s) -> None:
        """Цель (а с ней и запуск) удалили посреди работы — дальше не пишем ни строчки: это уже чужое."""
        if self.created is None:
            return
        same = s.scalar(select(Run.id).where(Run.id == self.run_id, Run.target_id == self.target.id,
                                             Run.created_at == self.created))
        if same is None:
            raise asyncio.CancelledError("запуск удалён")

    def log(self, message: str, level: str = "info") -> None:
        self._n += 1
        with self.ctx.db.session() as s:
            self._check_alive(s)
            s.add(RunLine(run_id=self.run_id, n=self._n, level=level if level in LEVELS else "info",
                          text=str(message)[:4000]))

    def progress(self, done: int, total: int, label: str = "") -> None:
        now = time.monotonic()
        if done < total and now - self._progress_at < 0.4:
            return
        self._progress_at = now
        with self.ctx.db.session() as s:
            run = s.get(Run, self.run_id)
            if run is not None:
                run.progress = {"done": done, "total": total, "label": label}

    # --- находки ---------------------------------------------------------
    async def emit(self, kind: str, value: str, **data: Any) -> bool:
        value = str(value)[:512]
        clean = _jsonable(data)
        with self.ctx.db.session() as s:
            self._check_alive(s)
            asset = s.scalar(select(Asset).where(Asset.target_id == self.target.id, Asset.kind == kind,
                                                 Asset.value == value))
            new = asset is None
            if new:
                s.add(Asset(target_id=self.target.id, kind=kind, value=value, data=clean, source=self.spec.id,
                            run_id=self.run_id))
            else:
                merged = dict(asset.data or {})
                merged.update(clean)
                asset.data = merged
                asset.last_seen = utcnow()
                asset.run_id = self.run_id
                asset.source = self.spec.id
        self.total += 1
        self.new += int(new)
        if kind == "ip":
            self._ips = None
        return new

    def assets(self, kind: str) -> list[dict[str, Any]]:
        with self.ctx.db.session() as s:
            rows = s.scalars(select(Asset).where(Asset.target_id == self.target.id, Asset.kind == kind)
                             .order_by(Asset.id)).all()
            return [{"value": a.value, "data": dict(a.data or {}), "first_seen": a.first_seen,
                     "last_seen": a.last_seen} for a in rows]

    # --- что входит в цель, и сеть ------------------------------------------
    def _ip_assets(self) -> dict[str, dict[str, Any]]:
        if self._ips is None:
            self._ips = {a["value"]: a["data"] for a in self.assets("ip")}
        return self._ips

    def allows(self, host: str, port: int | None = None) -> tuple[bool, str]:
        """Входит ли адрес в цель. IP, в который указывают имена цели, тоже входит —
        кроме чужой инфраструктуры (CDN): туда можно только как на сайт, на 80 и 443."""
        try:
            h = normalize_host(host)
        except ScopeError as exc:
            return False, str(exc)
        ok, reason = self.target.scope.explain(h)
        if ok or reason.startswith("Исключено"):
            return ok, reason
        if is_ip(h):
            data = self._ip_assets().get(h) or {}
            names = [d for d in data.get("domains") or [] if self.target.scope.allows(d)]
            if names:
                cdn = data.get("cdn")
                if cdn and port not in (80, 443):
                    return False, f"чужая инфраструктура ({cdn}): сюда можно только как на сайт, порты 80 и 443"
                if not data.get("checked") and not data.get("private") and port not in (80, 443):
                    # пока не знаем, чья это сеть, адрес может оказаться CDN — только как на сайт
                    return False, "чья это сеть, ещё не проверено («Провайдер»): пока можно только порты 80 и 443"
                return True, f"адрес {display_host(names[0])}"
        return False, reason

    def _vet_ip(self, ip: str, port: int) -> tuple[bool, str]:
        """IP, в который разрешилось имя цели: те же правила, что и для адреса, — исключения, CDN, непроверенная сеть."""
        ok, reason = self.target.scope.explain(ip)
        if reason.startswith("Исключено"):
            return False, f"{ip}: {reason}"
        if ok:
            return True, ""
        data = self._ip_assets().get(ip) or {}
        if data.get("cdn") and port not in (80, 443):
            return False, f"{ip}: чужая инфраструктура ({data['cdn']}), можно только порты 80 и 443"
        if not data.get("checked") and not data.get("private") and port not in (80, 443):
            return False, f"{ip}: чья это сеть, не проверено («Провайдер») — пока только порты 80 и 443"
        return True, ""

    async def connect(self, host: str, port: int, timeout: float = 3.0, **kw: Any):
        if self.spec.kind is not Kind.ACTIVE:
            raise OutOfScope("пассивный модуль не обращается к сайту")
        ok, reason = self.allows(host, port)
        if not ok:
            raise OutOfScope(f"{host}: {reason}")
        name = normalize_host(host)  # звоним ровно туда, что проверили
        addr = name
        if not is_ip(name):
            # имя входит в цель, но соединение идёт на IP из DNS — и он тоже проходит проверку
            infos = await asyncio.wait_for(
                asyncio.get_running_loop().getaddrinfo(name, port, type=socket.SOCK_STREAM), timeout)
            why = "имя не разрешилось"
            for *_rest, sockaddr in infos:
                good, why = self._vet_ip(sockaddr[0], port)
                if good:
                    addr = sockaddr[0]
                    break
            else:
                raise OutOfScope(f"{host} → {why}")
            if kw.get("ssl") is not None:
                kw.setdefault("server_hostname", name)
        return await asyncio.wait_for(asyncio.open_connection(addr, port, **kw), timeout)

    async def tcp_ping(self, host: str, port: int, timeout: float = 3.0) -> float:
        start = time.perf_counter()
        _reader, writer = await self.connect(host, port, timeout)
        ms = (time.perf_counter() - start) * 1000
        writer.close()
        with suppress(Exception):
            await asyncio.wait_for(writer.wait_closed(), 1)
        return ms

    @property
    def cancelled(self) -> bool:
        task = self.engine.tasks.get(self.run_id)
        return bool(task and task.cancelling())


def _report(fut: concurrent.futures.Future) -> None:
    """Цепочка упала вне модуля — пусть это будет видно в логе, а не пропадёт молча."""
    if not fut.cancelled() and fut.exception() is not None:
        log.error("Цепочка запусков упала", exc_info=fut.exception())


class Engine:
    def __init__(self, ctx: Context) -> None:
        self.ctx = ctx
        self.loop: asyncio.AbstractEventLoop | None = None
        self.thread: threading.Thread | None = None
        self.tasks: dict[int, asyncio.Task] = {}
        self._sem: asyncio.Semaphore | None = None
        self._lock = threading.Lock()

    # --- поток -----------------------------------------------------------
    def _ensure(self) -> asyncio.AbstractEventLoop:
        with self._lock:
            if self.loop is not None and self.thread is not None and self.thread.is_alive():
                return self.loop
            ready = threading.Event()

            def main() -> None:
                loop = asyncio.new_event_loop()
                asyncio.set_event_loop(loop)
                self.loop = loop
                self._sem = asyncio.Semaphore(max(1, int(self.ctx.settings.get("runs.parallel", 3))))
                ready.set()
                try:
                    loop.run_forever()
                finally:
                    pending = asyncio.all_tasks(loop)
                    for task in pending:
                        task.cancel()
                    with suppress(Exception):
                        loop.run_until_complete(asyncio.gather(*pending, return_exceptions=True))
                    loop.close()

            self.thread = threading.Thread(target=main, name="perimeter-runs", daemon=True)
            self.thread.start()
            ready.wait(5)
            assert self.loop is not None
            return self.loop

    def stop(self, timeout: float = 5.0) -> None:
        loop, thread = self.loop, self.thread
        if loop is None or thread is None:
            return
        for task in list(self.tasks.values()):
            loop.call_soon_threadsafe(task.cancel)
        loop.call_soon_threadsafe(loop.stop)
        thread.join(timeout)
        self.loop = self.thread = None

    def spawn(self, factory: Callable[[], Any]) -> concurrent.futures.Future:
        """Запустить любую корутину в потоке движка (проверка прокси и т. п.)."""
        return asyncio.run_coroutine_threadsafe(factory(), self._ensure())

    # --- запуски ---------------------------------------------------------
    def submit(self, run_ids: list[int]) -> concurrent.futures.Future:
        """Запуски из одного вызова идут по очереди: «запустить всё» = цепочка."""
        fut = asyncio.run_coroutine_threadsafe(self._chain(list(run_ids)), self._ensure())
        fut.add_done_callback(_report)
        return fut

    def busy(self) -> int:
        return sum(1 for t in self.tasks.values() if not t.done())

    def cancel(self, run_id: int) -> bool:
        """True — запуск уже шёл и получил сигнал остановки."""
        task = self.tasks.get(run_id)
        if task is not None and self.loop is not None and not task.done():
            self.loop.call_soon_threadsafe(task.cancel)
            return True
        return False

    async def _chain(self, run_ids: list[int]) -> None:
        broken: dict[str, str] = {}  # модуль цепочки, который не отработал → его название
        for run_id in run_ids:
            if self._skip_if_broken(run_id, broken):
                continue
            task = asyncio.create_task(self._run_one(run_id))
            self.tasks[run_id] = task
            await asyncio.wait([task])  # отмена одного запуска не обрывает цепочку
            self.tasks.pop(run_id, None)
            with self.ctx.db.session() as s:
                run = s.get(Run, run_id)
                if run is not None and run.status != "done":
                    module = self.ctx.modules.get(run.module_id)
                    broken[run.module_id] = module.spec.title if module else run.module_id

    def _skip_if_broken(self, run_id: int, broken: dict[str, str]) -> bool:
        """Модуль построен на том, что в этой цепочке не отработало, — пропускаем, а не гоняем впустую."""
        if not broken:
            return False
        from ..targets.models import Target

        with self.ctx.db.session() as s:
            run = s.get(Run, run_id)
            module = self.ctx.modules.get(run.module_id) if run is not None else None
            if run is None or module is None or run.status != "queued":
                return False
            missing = [broken[m] for m in module.spec.needs if m in broken]
            if not missing:
                return False
            why = "не отработал " + ", ".join(f"«{t}»" for t in missing)
            run.status, run.finished_at, run.error, run.summary = "skipped", utcnow(), why, f"пропущен: {why}"
            s.add(RunLine(run_id=run_id, n=1, level="warn", text=f"[skip] пропущен: {why}"))
            target = s.get(Target, run.target_id)
            self.ctx.audit(s, "run.skipped", f"{module.spec.title} {target.name if target else ''}: пропущен, {why}",
                           entity=("target", run.target_id), actor=run.actor, data={"run": run_id, "module": run.module_id})
            broken[run.module_id] = module.spec.title  # и то, что построено на нём, тоже пропустим
            return True

    async def _run_one(self, run_id: int) -> None:
        assert self._sem is not None
        async with self._sem:
            await self._execute(run_id)

    async def _execute(self, run_id: int) -> None:
        from ..targets.models import Target

        with self.ctx.db.session() as s:
            run = s.get(Run, run_id)
            if run is None or run.status != "queued":
                return
            target = s.get(Target, run.target_id)
            module = self.ctx.modules.get(run.module_id)
            if target is None or module is None:
                run.status, run.finished_at = "failed", utcnow()
                run.error = "цель удалена" if target is None else f"модуль «{run.module_id}» не найден"
                return
            view = TargetView(target.id, target.domain, target.scope)
            # «в очереди» → «идёт» одним UPDATE с условием: если его только что отменили, не начинаем
            claimed = s.execute(update(Run).where(Run.id == run_id, Run.status == "queued")
                                .values(status="running", started_at=utcnow())).rowcount
            if not claimed:
                return
            # недостающее или пустое — из значений по умолчанию модуля (и для старых запусков)
            params = {fld.key: fld.default for fld in module.spec.params}
            params.update({k: v for k, v in (run.params or {}).items() if v is not None})
            actor, created = run.actor, run.created_at

        rc = RunContext(self, run_id, module, view, params, created)
        spec = module.spec
        status, error = "done", ""
        started = time.monotonic()
        try:
            rc.log(f"$ perimeter run {spec.id} {view.name}", "dim")
            await asyncio.wait_for(module().run(rc), timeout=spec.timeout)
        except asyncio.CancelledError:
            status = "cancelled"
        except TimeoutError as exc:
            status = "failed"
            if time.monotonic() - started >= spec.timeout - 1:
                error = f"превышено время работы модуля: {spec.timeout} с"
            else:  # таймаут внутри модуля, который он не поймал сам
                log.exception("Модуль %s не поймал таймаут", spec.id)
                error = f"TimeoutError: {exc or 'сеть не ответила вовремя'}"
        except OutOfScope as exc:
            status, error = "failed", f"не входит в цель: {exc}"
        except ModuleError as exc:
            status, error = "failed", str(exc)
        except Exception as exc:  # noqa: BLE001 — ошибка модуля не роняет движок
            log.exception("Модуль %s упал", spec.id)
            status, error = "failed", f"{type(exc).__name__}: {exc}"

        seconds = time.monotonic() - started
        summary = rc.summary or (f"найдено {rc.total}, новых {rc.new}" if rc.total else "ничего не найдено")
        with suppress(Exception, asyncio.CancelledError):  # последняя строка консоли — не повод потерять итог
            if status == "done":
                rc.log(f"[done] {summary}  за {seconds:.1f} с", "ok")
            elif status == "cancelled":
                rc.log(f"[stop] остановлено через {seconds:.1f} с. {summary}", "warn")
            else:
                rc.log(f"[err] {error}", "err")
        await self._finish(run_id, created, status, error, summary, rc, spec, view, actor)

    async def _finish(self, run_id: int, created: Any, status: str, error: str, summary: str, rc: RunContext,
                      spec: Any, view: TargetView, actor: str) -> None:
        """Итог запуска записывается всегда — иначе он навсегда остался бы «идёт»."""
        for attempt in range(4):
            try:
                with self.ctx.db.session() as s:
                    run = s.get(Run, run_id)
                    if run is None or run.created_at != created:  # цель удалили вместе с запуском
                        return
                    run.status, run.finished_at, run.error = status, utcnow(), error
                    run.summary, run.found_new, run.found_total = summary, rc.new, rc.total
                    if status == "done":
                        run.progress = None
                    text = error if status == "failed" else summary
                    self.ctx.audit(s, f"run.{status}", f"{spec.title} {view.name}: {text}", entity=("target", view.id),
                                   actor=actor, data={"run": run_id, "module": spec.id})
                return
            except OperationalError:  # база занята дольше обычного — пробуем ещё раз
                log.warning("Итог запуска %s не записался, попытка %d", run_id, attempt + 1)
                await asyncio.sleep(0.5 * (attempt + 1))
        log.error("Итог запуска %s так и не записался", run_id)
