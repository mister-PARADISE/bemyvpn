"""Разведка: первые модули и то, как их находки выглядят в карточке цели.

Сводка цели («паспорт») и подсказки «можно добавить» собираются из находок —
сами модули о них ничего не знают.
"""

from __future__ import annotations

from collections import Counter

from fastapi import APIRouter, HTTPException, Request
from sqlalchemy import select

from ...core import fields as f
from ...core.context import Context, Slot
from ...core.modules import AssetType
from ...core.settings import SettingsSection
from ...web import DB, actor, get_ctx, redirect
from ..runs.models import Asset
from . import netkit
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
            ("время", lambda a: _ms(a.data.get("ms")))],
    "certificate": [("хост", lambda a: a.value), ("выдан", lambda a: a.data.get("issuer")), ("действует до", _days),
                    ("имён", lambda a: str(len(a.data.get("sans") or [])))],
    "subdomain": [("имя", lambda a: a.value), ("откуда", lambda a: a.data.get("source"))],
    "domain": [("имя", lambda a: a.value), ("откуда", lambda a: a.data.get("source"))],
    "neighbor": [("адрес", lambda a: a.value), ("имя", lambda a: a.data.get("ptr")),
                 ("рядом с", lambda a: a.data.get("near")),
                 ("", lambda a: ("похоже, ваш", "warn") if a.data.get("related") else "")],
}


# --- сводка цели ------------------------------------------------------------------

def passport(db, ctx: Context, target=None, **_):
    if target is None:
        return None
    rows = db.scalars(select(Asset).where(Asset.target_id == target.id)).all()
    by: dict[str, list[Asset]] = {}
    for a in rows:
        by.setdefault(a.kind, []).append(a)
    ips = [a for a in by.get("ip", []) if (a.data or {}).get("domains") or target.scope.allows(a.value)]
    lines: list[tuple[str, str, str]] = []
    if ips:
        main = ips[0]
        lines.append(("ip", main.value + (f"  (+{len(ips) - 1})" if len(ips) > 1 else ""), ""))
        own = [a for a in ips if not a.data.get("cdn")]
        cdn = sorted({a.data["cdn"] for a in ips if a.data.get("cdn")})
        host = next((a for a in own if a.data.get("as_name")), None)
        if host:
            lines.append(("хостинг", f"{netkit.org_name(host.data['as_name'])}  AS{host.data.get('asn')}  {host.data.get('country') or ''}".strip(), ""))
        lines.append(("cdn", ", ".join(cdn) if cdn else "нет", "warn" if cdn else ""))
    ns = [a for a in by.get("dns_record", []) if a.data.get("type") == "NS"]
    mx = [a for a in by.get("dns_record", []) if a.data.get("type") == "MX"]
    if ns:
        provider = Counter(a.data.get("provider") for a in ns if a.data.get("provider")).most_common(1)
        lines.append(("dns", provider[0][0] if provider else ns[0].data.get("host", ""), ""))
    if ns or mx:
        provider = next((a.data.get("provider") for a in mx if a.data.get("provider")), None)
        lines.append(("почта", provider or (mx[0].data.get("host") if mx else "нет MX"), ""))
    rtts = [a.data["rtt_ms"] for a in ips if a.data.get("rtt_ms") is not None]
    if rtts:
        lines.append(("пинг", _ms(min(rtts)), ""))
    elif any(a.data.get("reachable") is False for a in ips):
        lines.append(("пинг", "не отвечает", "err"))
    urls = by.get("url", [])
    if urls:
        u = next((x for x in urls if x.value.startswith("https://")), urls[0])
        lines.append(("веб", f"{u.data.get('status')}  {u.data.get('server') or ''}".strip(),
                      "" if 200 <= (u.data.get("status") or 0) < 400 else "err"))
    certs = [a for a in by.get("certificate", [])]
    if certs:
        bad = [c for c in certs if c.data.get("valid") is False]
        days = [c.data["days_left"] for c in certs if c.data.get("days_left") is not None]
        if bad:
            lines.append(("tls", bad[0].data.get("error") or "невалиден", "err"))
        elif days:
            d = min(days)
            lines.append(("tls", f"ещё {d} дн", "err" if d < 7 else "warn" if d < 21 else ""))
    ports = [a for a in by.get("port", []) if a.data.get("state") != "closed"]
    if ports:
        nums = sorted({a.data.get("port") for a in ports if a.data.get("port")})
        risky = [a for a in ports if a.data.get("risky")]
        lines.append(("порты", " ".join(str(n) for n in nums[:16]) + (" …" if len(nums) > 16 else ""),
                      "err" if risky else ""))
    neighbors = by.get("neighbor", [])
    if neighbors:
        rel = sum(1 for a in neighbors if a.data.get("related"))
        lines.append(("соседи", f"{len(neighbors)} с именами" + (f", похожих на ваши {rel}" if rel else ""),
                      "warn" if rel else ""))
    updated = max((a.last_seen for a in rows), default=None)
    return {"lines": lines, "updated": updated}


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
    return {"items": items[:20]} if items else None


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
    ctx.add_slot("target.main", Slot("passport", "recon/_passport.html", data=passport, order=10))
    ctx.add_slot("target.side", Slot("suggest", "recon/_suggest.html", data=suggestions, order=20))
    ctx.settings.section(SettingsSection(
        id="recon", title="Разведка", icon="search", group="Модули", order=20,
        description="Через какие DNS-серверы модули спрашивают про адреса, сети и соседей.",
        fields=(
            f.lines("dns_servers", "DNS-серверы", placeholder="1.1.1.1\n8.8.8.8",
                    help="По одному в строке, можно с портом: 127.0.0.1:5353. Пусто — системные"),
            f.integer("dns_timeout", "Ждать ответа DNS, с", default=3, min=1, max=15),
        ),
    ))
