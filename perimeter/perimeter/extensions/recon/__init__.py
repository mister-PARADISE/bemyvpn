"""Разведка: модули (один модуль — один вопрос) и то, как их находки выглядят на странице цели.

Сводка, замечания, структура и подсказки «можно добавить» собираются из находок
(recon/view.py) — сами модули о них ничего не знают.
"""

from __future__ import annotations


from fastapi import APIRouter, HTTPException, Request
from sqlalchemy import select

from ...core import fields as f
from ...core.context import Context, Slot, Stat
from ...core.modules import AssetType
from ...core.settings import SettingsSection
from ...web import DB, actor, get_ctx, redirect
from ...web.ui import plural
from ..runs.models import Asset
from . import feeds, netkit, view, vulndb
from ..targets.models import Target
from .modules import MODULES

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


def _tech_cves(a):
    d = a.data or {}
    if not d.get("cve_checked"):
        return "—" if d.get("version") else ("версия не видна", "")
    if not d.get("cves"):
        return ("нет", "ok")
    tone = "err" if d.get("critical") or d.get("high") or d.get("kev") else "warn"
    return (f"{d['cves']}" + (f" (крит. {d['critical']})" if d.get("critical") else ""), tone)


def _vuln_score(a):
    d = a.data or {}
    if d.get("cvss") is None:
        return ("—", "")
    tone = {"critical": "err", "high": "err", "medium": "warn"}.get(d.get("severity"), "")
    return (f"{d['cvss']:.1f}", tone)


def _vuln_flags(a):
    d = a.data or {}
    flags = (["в атаках"] if d.get("kev") else []) + (["эксплойт"] if d.get("exploit") else [])
    if d.get("epss") is not None and d["epss"] >= 0.01:
        flags.append(f"EPSS {d['epss'] * 100:.0f}%")
    return (", ".join(flags), "err" if d.get("kev") else "warn") if flags else ""


def _tech_support(a):
    tag = view.support_tag(a.data or {})
    return tag if tag else ""


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
    "technology": [("программа", lambda a: a.value), ("вид", lambda a: a.data.get("category")),
                   ("где", lambda a: ", ".join((a.data.get("where") or [])[:2])),
                   ("сборка", lambda a: " ".join(x for x in (a.data.get("distro"), a.data.get("package")) if x)),
                   ("уязвимости", _tech_cves), ("поддержка", _tech_support),
                   ("", lambda a: ("больше не видно", "") if a.data.get("state") == "gone"
                    else ("убран из каталога WordPress", "warn") if a.data.get("closed") else "")],
    "vulnerability": [("ID", lambda a: a.value), ("программа", lambda a: f"{a.data.get('product_title', '')} {a.data.get('version', '')}"),
                      ("CVSS", _vuln_score), ("опасность", lambda a: vulndb.SEVERITY_RU.get(a.data.get("severity", ""), "")),
                      ("признаки", _vuln_flags), ("исправлено в", lambda a: a.data.get("fixed_in")),
                      ("", lambda a: ("закрыта", "ok") if a.data.get("state") == "closed" else "")],
    "neighbor": [("адрес", lambda a: a.value), ("имя", lambda a: a.data.get("ptr")),
                 ("рядом с", lambda a: a.data.get("near")),
                 ("", lambda a: ("похоже, ваш", "warn") if a.data.get("related") else "")],
}


# --- страница цели: сводка, замечания, структура ------------------------------------

def summary_slot(db, ctx: Context, target=None, **_):
    if target is None:
        return None
    f = view.load(db, target)
    # ни один модуль ещё не отрабатывал — вместо сетки «▶» одна кнопка «Запустить всё»
    return {"groups": view.summary(f) if (f.rows or f.ran) else [], "updated": f.updated}


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


def vulns_slot(db, ctx: Context, target=None, **_):
    if target is None:
        return None
    f = view.load(db, target)
    if not f.vulns and not f.did_cve():
        return None
    groups = view.vuln_groups(f)
    hot = any(g["hot"] for g in groups)
    return {"groups": groups, "unchecked": view.unchecked(f), "total": len(f.vulns), "totals": view.vuln_totals(f),
            "nav_count": len(f.vulns), "nav_tone": "err" if hot else "warn" if groups else "ok"}


def tree_slot(db, ctx: Context, target=None, **_):
    if target is None:
        return None
    f = view.load(db, target)
    if not f.rows:
        return None
    return {"root": view.tree(f)}


# --- список целей и обзор --------------------------------------------------------------

def target_columns(db, targets) -> list[dict]:
    """Две колонки списка целей: где сайт живёт и что с ним не так."""
    facts = view.facts_for(db, targets)
    addr: dict[int, dict] = {}
    problems: dict[int, dict] = {}
    for tid, fx in facts.items():
        if fx.ips:
            first = next(iter(fx.ips.values()))
            d = first.data or {}
            more = len(fx.ips) - 1
            where = (f"за {d['cdn']}" if d.get("cdn") else netkit.org_name(d.get("as_name")) if d.get("asn")
                     else "частная сеть" if d.get("private") else "провайдер не проверен")
            addr[tid] = {"l1": [(first.value, "")] + ([(f"+{more}", "dim")] if more else []), "l2": where}
        elif fx.did("resolve") and fx.ran is not None and "resolve" in fx.ran:
            addr[tid] = {"l1": [("не разрешается", "err")], "l2": "у имён нет адресов"}
        if not fx.ran:
            continue
        found = view.issues(fx)
        c = view.counts(found)
        segs = ([(f"[!] {c['err']}", "err")] if c["err"] else []) + ([(f"[~] {c['warn']}", "warn")] if c["warn"] else []) \
            + ([(f"[i] {c['info']}", "dim")] if c["info"] else [])
        top = next((i.title for i in found if i.level in ("err", "warn")), "")
        problems[tid] = {"l1": segs or [("[ok]", "ok")], "l2": top or ("к сведению" if c["info"] else "замечаний нет")}
    return [{"title": "Адрес", "order": 10, "cells": addr}, {"title": "Замечания", "order": 20, "cells": problems}]


def issues_stat(db, ctx: Context):
    """Строка «Панели» на обзоре: серьёзное по всем целям сразу."""
    targets = list(db.scalars(select(Target)))
    total = {"err": 0, "warn": 0}
    sites = 0
    for fx in view.facts_for(db, targets).values():
        if not fx.ran:
            continue
        c = view.counts(view.issues(fx))
        total["err"] += c["err"]
        total["warn"] += c["warn"]
        sites += bool(c["err"])
    if not total["err"] and not total["warn"]:
        return "серьёзных нет", "ok"
    text = (f"серьёзных {total['err']} на {sites} {plural(sites, 'цели', 'целях', 'целях')}" if total["err"]
            else "серьёзных нет")
    if total["warn"]:
        text += f", стоит поправить {total['warn']}"
    return text, "err" if total["err"] else "warn"


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
    ctx.asset_types.add(AssetType("vulnerability", "Уязвимость", "shield-alert",
                                  "Известная уязвимость (CVE) в найденной версии программы", 5))
    for module in MODULES:
        ctx.modules.add(module)
    ctx.services.setdefault("asset_columns", {}).update(COLUMNS)
    ctx.add_router(router)
    ctx.add_slot("target.main", Slot("summary", "recon/_summary.html", data=summary_slot, order=10, nav="сводка"))
    ctx.add_slot("target.main", Slot("issues", "recon/_issues.html", data=issues_slot, order=12, nav="замечания"))
    ctx.add_slot("target.main", Slot("vulns", "recon/_vulns.html", data=vulns_slot, order=13, nav="уязвимости"))
    ctx.add_slot("target.main", Slot("tree", "recon/_tree.html", data=tree_slot, order=14, nav="структура"))
    ctx.add_slot("target.side", Slot("suggest", "recon/_suggest.html", data=suggestions, order=20, nav="можно добавить"))
    ctx.hook("targets.columns", target_columns)
    ctx.stats.add(Stat("issues", "замечания", issues_stat, order=15, link="/targets"))
    ctx.settings.section(SettingsSection(
        id="recon", title="Разведка", icon="search", group="Модули", order=20,
        description="Через какие DNS-серверы модули спрашивают про адреса, сети и соседей.",
        fields=(
            f.lines("dns_servers", "DNS-серверы", placeholder="1.1.1.1\n8.8.8.8",
                    help="По одному в строке, можно с портом: 127.0.0.1:5353. Пусто — системные"),
            f.integer("dns_timeout", "Ждать ответа DNS, с", default=3, min=1, max=15),
        ),
    ))
    ctx.settings.section(SettingsSection(
        id="vulndb", title="Уязвимости", icon="shield-alert", group="Модули", order=25,
        description="Откуда модули «Уязвимости» и «Поддержка версий» берут данные: NVD — все CVE по программе, "
                    "база WordPress — плагины и темы, CISA KEV — что уже используют в атаках, EPSS — вероятность "
                    "атаки, endoflife.date — сроки поддержки веток. Базы хранятся в data/vulndb и обновляются сами "
                    "(«Модули → Данные»).",
        fields=(
            f.secret("nvd_api_key", "Ключ API NVD",
                     help="Необязательно. Без ключа NVD отвечает не чаще 5 раз за 30 секунд — первая проверка "
                          "идёт дольше. Ключ бесплатный: nvd.nist.gov/developers/request-an-api-key"),
            f.integer("cache_hours", "Обновлять базы раз в, ч", default=24, min=1, max=720, width="half",
                      help="Обновляет сама, в фоне; после обновления цели перепроверяются («Настройки → Данные модулей»)"),
            f.boolean("use_kev", "Отмечать уязвимости, которые уже используют в атаках (CISA KEV)", default=True),
            f.boolean("use_epss", "Вероятность атаки в ближайший месяц (EPSS)", default=True),
            f.url("nvd_url", "Адрес NVD API", default="https://services.nvd.nist.gov/rest/json/cves/2.0"),
            f.url("kev_url", "Каталог CISA KEV",
                  default="https://www.cisa.gov/sites/default/files/feeds/known_exploited_vulnerabilities.json"),
            f.url("epss_url", "Адрес EPSS API", default="https://api.first.org/data/v1/epss"),
            f.url("wp_url", "База плагинов и тем WordPress", default="https://www.wpvulnerability.net",
                  help="Уязвимости плагинов по их названию: собирает Wordfence, Patchstack, WPScan и NVD"),
            f.url("eol_url", "Сроки поддержки версий (endoflife.date)", default="https://endoflife.date/api"),
        ),
    ))
    feeds.register(ctx)
