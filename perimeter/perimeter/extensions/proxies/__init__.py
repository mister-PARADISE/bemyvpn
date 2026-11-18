"""Прокси: свой список, проверка (жив ли, отклик, куда выходит), строка на обзоре.

Вставляются пачкой в любом привычном виде — по одному в строке. Проверка идёт
в потоке движка запусков, страница сама обновляется, пока она не закончится.
Модули потом смогут брать живой прокси через ctx.services["proxies"].pick().
"""

from __future__ import annotations

import asyncio
import math
import random
from urllib.parse import quote

from fastapi import APIRouter, HTTPException, Request
from sqlalchemy import func, select

from ...core import fields as f
from ...core.context import Context, Page, SearchHit, Stat
from ...core.db import utcnow
from ...core.settings import SettingsSection
from ...web import DB, actor, flash, form_data, get_ctx, redirect, render
from ...web.ui import plural
from .checker import ProxyParseError, check, parse_proxy
from .models import STATES, Proxy

TITLE = "Прокси"
DESCRIPTION = "Список прокси и их проверка"
VERSION = "1.0"
PER_PAGE = 100

router = APIRouter()

TABS = (("", "Все"), ("alive", "Живые"), ("dead", "Мёртвые"), ("unknown", "Не проверены"))


def _proxy_or_404(db, proxy_id: int) -> Proxy:
    proxy = db.get(Proxy, proxy_id)
    if proxy is None:
        raise HTTPException(404, "Прокси не найден")
    return proxy


# --- проверка ------------------------------------------------------------------

async def check_many(ctx: Context, ids: list[int]) -> None:
    url = ctx.settings.get("proxies.check_url")
    timeout = float(ctx.settings.get("proxies.timeout"))
    sem = asyncio.Semaphore(int(ctx.settings.get("proxies.parallel")))
    with ctx.db.session() as s:
        targets = {p.id: p.url for p in s.scalars(select(Proxy).where(Proxy.id.in_(ids)))}

    async def one(pid: int, proxy_url: str) -> None:
        async with sem:
            try:
                result = await check(url, proxy_url, timeout)
            except Exception as exc:  # noqa: BLE001 — итог нужен всегда, иначе прокси навсегда «проверяется»
                result = {"status": "dead", "error": f"проверка не удалась: {exc}"[:200]}
        with ctx.db.session() as s:
            p = s.get(Proxy, pid)
            if p is None:
                return
            p.status, p.error, p.checked_at = result["status"], result.get("error", ""), utcnow()
            p.latency_ms = result.get("latency_ms")
            if result["status"] == "alive":
                p.exit_ip = result.get("exit_ip", "")
                p.country = result.get("country", "")
                p.city = result.get("city", "")
                p.org = result.get("org", "")

    await asyncio.gather(*(one(pid, u) for pid, u in targets.items()))


def start_check(request: Request, db, proxies: list[Proxy]) -> int:
    if not proxies:
        return 0
    for p in proxies:
        p.status = "checking"
    db.commit()
    ctx = get_ctx(request)
    ids = [p.id for p in proxies]
    ctx.services["engine"].spawn(lambda: check_many(ctx, ids))
    return len(ids)


# --- страницы ------------------------------------------------------------------

@router.get("/proxies")
def proxies_page(request: Request, db: DB, state: str = "", q: str = "", page: int = 1):
    counts = dict(db.execute(select(Proxy.status, func.count()).group_by(Proxy.status)).all())
    total_all = sum(counts.values())
    stmt = select(Proxy)
    if state in STATES:
        stmt = stmt.where(Proxy.status == state)
    if q.strip():
        like = f"%{q.strip()}%"
        stmt = stmt.where(Proxy.host.ilike(like) | Proxy.label.ilike(like) | Proxy.country.ilike(like)
                          | Proxy.org.ilike(like) | Proxy.exit_ip.ilike(like))
    total = db.scalar(select(func.count()).select_from(stmt.subquery())) or 0
    pages = max(1, math.ceil(total / PER_PAGE))
    page = min(max(page, 1), pages)
    order = (Proxy.status != "alive", Proxy.latency_ms.is_(None), Proxy.latency_ms, Proxy.id)
    rows = db.scalars(stmt.order_by(*order).offset((page - 1) * PER_PAGE).limit(PER_PAGE)).all()
    tab_counts = {"": total_all, **{k: counts.get(k, 0) for k in STATES}}
    return render(request, "proxies/index.html", proxies=rows, state=state, q=q, page=page, pages=pages, total=total,
                  total_all=total_all, alive=counts.get("alive", 0), checking=counts.get("checking", 0),
                  tabs=[(k, t, tab_counts[k]) for k, t in TABS], query={"state": state, "q": q})


@router.post("/proxies")
async def proxies_add(request: Request, db: DB):
    form = await form_data(request)
    scheme = str(form.get("scheme") or "http")
    label = str(form.get("label") or "").strip()[:120]
    added, dupes, bad = [], 0, []
    seen: set[tuple] = set()
    for line in str(form.get("lines", "")).splitlines():
        if not line.strip() or line.strip().startswith("#"):
            continue
        try:
            p = parse_proxy(line, scheme)
        except ProxyParseError as exc:
            bad.append(f"«{line.strip()[:40]}»: {exc}")
            continue
        key = (p.scheme, p.host, p.port, p.username)
        exists = db.scalar(select(Proxy.id).where(Proxy.scheme == p.scheme, Proxy.host == p.host,
                                                  Proxy.port == p.port, Proxy.username == p.username))
        if exists or key in seen:
            dupes += 1
            continue
        seen.add(key)
        proxy = Proxy(scheme=p.scheme, host=p.host, port=p.port, username=p.username, password=p.password,
                      label=label)
        db.add(proxy)
        added.append(proxy)
    db.flush()
    if added:
        get_ctx(request).audit(db, "proxy.added", f"Добавлено прокси: {len(added)}", actor=actor(request))
    msg = [f"добавлено {len(added)}"]
    if dupes:
        msg.append(f"уже были {dupes}")
    if bad:
        msg.append(f"не разобрал {len(bad)}: " + "; ".join(bad[:3]) + (" …" if len(bad) > 3 else ""))
    if added and form.get("check"):
        start_check(request, db, added)
        msg.append("проверяю")
    flash(request, ", ".join(msg), "error" if bad and not added else "success" if added else "info")
    return redirect("/proxies")


@router.post("/proxies/check")
async def proxies_check(request: Request, db: DB):
    which = str((await form_data(request)).get("which", "all"))
    stmt = select(Proxy).where(Proxy.status != "checking")
    if which == "dead":
        stmt = stmt.where(Proxy.status == "dead")
    n = start_check(request, db, list(db.scalars(stmt)))
    flash(request, f"Проверяю {n} {plural(n, 'прокси', 'прокси', 'прокси')}" if n else "Нечего проверять", "info")
    return redirect("/proxies")


@router.post("/proxies/{proxy_id}/check")
def proxy_check(request: Request, db: DB, proxy_id: int):
    start_check(request, db, [_proxy_or_404(db, proxy_id)])
    return redirect("/proxies")


@router.post("/proxies/{proxy_id}/delete")
def proxy_delete(request: Request, db: DB, proxy_id: int):
    proxy = _proxy_or_404(db, proxy_id)
    name = proxy.display
    db.delete(proxy)
    get_ctx(request).audit(db, "proxy.deleted", f"Удалён прокси {name}", actor=actor(request))
    flash(request, f"Удалён {name}")
    return redirect("/proxies")


@router.post("/proxies/delete-dead")
def proxies_delete_dead(request: Request, db: DB):
    dead = list(db.scalars(select(Proxy).where(Proxy.status == "dead")))
    for p in dead:
        db.delete(p)
    if dead:
        get_ctx(request).audit(db, "proxy.deleted", f"Удалено мёртвых прокси: {len(dead)}", actor=actor(request))
    flash(request, f"Удалено мёртвых: {len(dead)}", "success" if dead else "info")
    return redirect("/proxies")


# --- обзор, поиск, сервис для модулей ------------------------------------------------

def proxies_stat(db, ctx: Context):
    counts = dict(db.execute(select(Proxy.status, func.count()).group_by(Proxy.status)).all())
    total = sum(counts.values())
    if not total:
        return "нет"
    alive = counts.get("alive", 0)
    text = f"{alive} {plural(alive, 'живой', 'живых', 'живых')} из {total}"
    if counts.get("checking"):
        text += f", проверяется {counts['checking']}"
    return text, "ok" if alive else "warn"


def search_proxies(db, q: str) -> list[SearchHit]:
    like = f"%{q}%"
    rows = db.scalars(select(Proxy).where(Proxy.host.ilike(like) | Proxy.label.ilike(like)).limit(5)).all()
    return [SearchHit(p.hostport, f"/proxies?q={quote(p.host)}", "Прокси", p.state_title, "server") for p in rows]


class ProxyPool:
    def __init__(self, ctx: Context) -> None:
        self.ctx = ctx

    def pick(self) -> str | None:
        """Случайный живой прокси (URL с логином) или None."""
        with self.ctx.db.session() as s:
            urls = [p.url for p in s.scalars(select(Proxy).where(Proxy.status == "alive"))]
        return random.choice(urls) if urls else None


def on_ready(ctx: Context) -> None:
    with ctx.db.session() as s:  # проверка оборвалась вместе с панелью
        for p in s.scalars(select(Proxy).where(Proxy.status == "checking")):
            p.status = "unknown"


def setup(ctx: Context) -> None:
    ctx.pages.add(Page("proxies", "Прокси", "/proxies", icon="server", section="Инструменты", order=12,
                       description="Свои прокси: живы ли, отклик, страна выхода"))
    ctx.add_router(router)
    ctx.stats.add(Stat("proxies", "прокси", proxies_stat, order=40, link="/proxies"))
    ctx.search(search_proxies)
    ctx.services["proxies"] = ProxyPool(ctx)
    ctx.hook("app.ready", on_ready)
    ctx.settings.section(SettingsSection(
        id="proxies", title="Прокси", icon="server", group="Модули", order=30,
        description="Как проверять прокси: через какой сайт узнавать адрес выхода и сколько ждать.",
        fields=(
            f.url("check_url", "Сайт для проверки", default="https://ipinfo.io/json",
                   help="Должен вернуть IP выхода: ipinfo.io, ip-api.com, api.ipify.org…"),
            f.integer("timeout", "Ждать ответа, с", default=10, min=2, max=60, width="half"),
            f.integer("parallel", "Проверять одновременно", default=20, min=1, max=200, width="half"),
        ),
    ))
