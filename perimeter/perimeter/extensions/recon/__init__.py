"""Разведка: первые модули и то, как их находки выглядят в карточке цели.

Сводка цели («паспорт») и подсказки «можно добавить» собираются из находок —
сами модули о них ничего не знают.
"""

from __future__ import annotations


from fastapi import APIRouter, HTTPException, Request
from sqlalchemy import select

from ...core import fields as f
from ...core.context import Context, Slot
from ...core.modules import AssetType
from ...core.settings import SettingsSection
from ...web import DB, actor, get_ctx, redirect
from ..runs.models import Asset
from . import netkit, view
from ..targets.models import Target
from .modules import HostInfo, Neighbors, OpenPorts, Reach

TITLE = "Разведка"
DESCRIPTION = "Хост и провайдер, пинг и веб, открытые порты, соседи по подсети"
VERSION = "1.0"

router = APIRouter()


# --- как показывать находки ---------------------------------------------------------

def _ms(v) -> str:
    return netkit.fmt_ms(v) if isinstance(v, (int, float)) else "—"


def _ip_net(a):
    d = a.data or {}
    if d.get("private"):
        return "частный адрес"
    return f"{netkit.org_name(d.get('as_name'))}  AS{d['asn']}".strip() if d.get("asn") else "—"


def _ip_ping(a):
    d = a.data or {}
    if d.get("reachable") is False:
        return ("не отвечает", "err")
    if d.get("rtt_ms") is not None:
        return f"{_ms(d['rtt_ms'])}  tcp/{d.get('rtt_port')}"
    return "—"


def _port_service(a):
    d = a.data or {}
    s = d.get("service") or "?"
    return (f"{s} — наружу!", "err") if d.get("risky") and d.get("state") != "closed" else s


def _days(a):
    d = a.data or {}
    if d.get("valid") is False:
        return (d.get("error") or "невалиден", "err")
    days = d.get("days_left")
    if days is None:
        return "—"
    return (f"{d.get('expires')}, {days} дн", "err" if days < 7 else "warn" if days < 21 else "")


COLUMNS = {
    "ip": [("адрес", lambda a: a.value), ("сеть", _ip_net), ("префикс", lambda a: a.data.get("prefix")),
           ("страна", lambda a: a.data.get("country")),
           ("cdn", lambda a: (a.data["cdn"], "warn") if a.data.get("cdn") else "нет"),
           ("ptr", lambda a: a.data.get("ptr")), ("пинг", _ip_ping)],
    "port": [("адрес", lambda a: a.value), ("сервис", _port_service), ("ответ", lambda a: a.data.get("banner")),
             ("время", lambda a: _ms(a.data.get("rtt_ms"))),
             ("состояние", lambda a: ("закрыт", "") if a.data.get("state") == "closed" else ("открыт", "ok"))],
    "dns_record": [("запись", lambda a: a.value), ("провайдер", lambda a: a.data.get("provider"))],
    "url": [("адрес", lambda a: a.value),
            ("код", lambda a: (str(a.data.get("status")), "ok" if 200 <= (a.data.get("status") or 0) < 400 else "err")),
            ("сервер", lambda a: a.data.get("server")), ("заголовок", lambda a: a.data.get("title")),
            ("куда", lambda a: ("→ https", "ok") if a.data.get("to_https") else a.data.get("location")),
            ("время", lambda a: _ms(a.data.get("ms")))],
    "certificate": [("хост", lambda a: a.value), ("выдан", lambda a: a.data.get("issuer")), ("действует до", _days),
                    ("имён", lambda a: str(len(a.data.get("sans") or [])))],
    "subdomain": [("имя", lambda a: a.value), ("откуда", lambda a: a.data.get("source"))],
    "domain": [("имя", lambda a: a.value), ("откуда", lambda a: a.data.get("source"))],
    "neighbor": [("адрес", lambda a: a.value), ("имя", lambda a: a.data.get("ptr")),
                 ("рядом с", lambda a: a.data.get("near")),
                 ("", lambda a: ("похоже, ваш", "warn") if a.data.get("related") else "")],
}


# --- страница цели: сводка, замечания, структура ------------------------------------

def summary_slot(db, ctx: Context, target=None, **_):
    if target is None:
        return None
    f = view.load(db, target)
    groups = view.summary(f)
    return {"groups": groups if any(g.facts for g in groups) else [], "updated": f.updated}


def issues_slot(db, ctx: Context, target=None, **_):
    if target is None:
        return None
    f = view.load(db, target)
    if not f.rows:
        return None
    found = view.issues(f)
    levels = {i.level for i in found}
    return {"issues": found, "nav_count": len(found),
            "nav_tone": "err" if "err" in levels else "warn" if "warn" in levels else "ok"}


def tree_slot(db, ctx: Context, target=None, **_):
    if target is None:
        return None
    f = view.load(db, target)
    if not f.rows:
        return None
    return {"root": view.tree(f)}


# --- можно добавить в цель ----------------------------------------------------------

def suggestions(db, ctx: Context, target=None, **_):
    if target is None:
        return None
    scope = target.scope
    rows = db.scalars(select(Asset).where(Asset.target_id == target.id, Asset.kind.in_(("neighbor", "domain")))).all()
    items = []
    for a in rows:
        d = a.data or {}
        if d.get("dismissed") or scope.allows(a.value):
            continue
        if a.kind == "neighbor" and d.get("related"):
            items.append({"id": a.id, "value": a.value, "why": f"{d.get('ptr')}, сосед {d.get('near')}"})
        elif a.kind == "domain":
            items.append({"id": a.id, "value": a.value, "why": f"есть в сертификате {d.get('seen_on', '')}".strip()})
    return {"items": items[:20], "nav_count": len(items), "nav_tone": "warn"} if items else None


@router.post("/targets/{target_id}/assets/{asset_id}/dismiss")
def dismiss(request: Request, db: DB, target_id: int, asset_id: int):
    asset = db.get(Asset, asset_id)
    if asset is None or asset.target_id != target_id:
        raise HTTPException(404, "Находка не найдена")
    asset.data = {**(asset.data or {}), "dismissed": True}
    target = db.get(Target, target_id)
    get_ctx(request).audit(db, "target.suggestion_hidden", f"{target.name}: скрыта подсказка {asset.value}",
                           entity=("target", target_id), actor=actor(request))
    return redirect(f"/targets/{target_id}#hosts")


def setup(ctx: Context) -> None:
    ctx.asset_types.add(AssetType("neighbor", "Сосед", "network", "Адрес в той же сети с обратным именем", 110))
    for module in (HostInfo, Reach, OpenPorts, Neighbors):
        ctx.modules.add(module)
    ctx.services.setdefault("asset_columns", {}).update(COLUMNS)
    ctx.add_router(router)
    ctx.add_slot("target.main", Slot("summary", "recon/_summary.html", data=summary_slot, order=10, nav="сводка"))
    ctx.add_slot("target.main", Slot("issues", "recon/_issues.html", data=issues_slot, order=12, nav="замечания"))
    ctx.add_slot("target.main", Slot("tree", "recon/_tree.html", data=tree_slot, order=14, nav="структура"))
    ctx.add_slot("target.side", Slot("suggest", "recon/_suggest.html", data=suggestions, order=20, nav="можно добавить"))
    ctx.settings.section(SettingsSection(
        id="recon", title="Разведка", icon="search", group="Модули", order=20,
        description="Через какие DNS-серверы модули спрашивают про адреса, сети и соседей.",
        fields=(
            f.lines("dns_servers", "DNS-серверы", placeholder="1.1.1.1\n8.8.8.8",
                    help="По одному в строке, можно с портом: 127.0.0.1:5353. Пусто — системные"),
            f.integer("dns_timeout", "Ждать ответа DNS, с", default=3, min=1, max=15),
        ),
    ))
