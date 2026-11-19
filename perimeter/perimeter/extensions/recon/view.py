"""Как показать цель человеку: сводка, замечания, структура.

Всё строится из находок (Asset), в сеть ничего не ходит. Модули пишут факты, а здесь решается,
что из них главное, что плохо и как это связано: имя → адрес → порт → что на нём отвечает.
Каждая строка сводки знает свой модуль: пока он не запускался, вместо значения — кнопка «▶ модуль».
"""

from __future__ import annotations

import ipaddress
import re
from collections import Counter
from dataclasses import dataclass, field
from datetime import datetime
from typing import Any
from urllib.parse import urlsplit

from sqlalchemy import func, select

from ...core.db import utcnow
from ...web.ui import plural
from ..runs.models import Asset, Run
from . import netkit, versions
from .ports import RISKY, SERVICES

Seg = tuple[str, str]  # (текст, оттенок: "" | ok | warn | err | dim)

REDIRECTS = (301, 302, 303, 307, 308)
TITLES = {"resolve": "Адреса", "asn": "Провайдер", "ptr": "Обратные имена", "zone": "DNS-зона", "mail": "Почта",
          "neighbors": "Соседи по подсети", "ping": "Пинг", "http": "Ответ сайта", "tls": "Сертификат",
          "ports": "Открытые порты"}


def _d(a: Asset | None) -> dict[str, Any]:
    return (a.data or {}) if a is not None else {}


def _ip_key(ip: str):
    try:
        addr = ipaddress.ip_address(ip)
        return (addr.version, addr)
    except ValueError:
        return (9, ip)


def anchor(*parts: Any) -> str:
    """Якорь узла дерева: n-127-0-0-2-3306."""
    return "n-" + re.sub(r"[^0-9a-zа-яё]+", "-", "-".join(str(p) for p in parts).lower()).strip("-")


def _status_tone(code: Any) -> str:
    code = code or 0
    return "err" if code >= 500 or code == 0 else "warn" if code >= 400 else ""


def _days_tone(days: int | None) -> str:
    return "" if days is None else "err" if days < 7 else "warn" if days < 21 else ""


_SEV_ORDER = {"critical": 0, "high": 1, "medium": 2, "low": 3, "none": 4, "unknown": 5}


def _vuln_rank(a: Asset) -> tuple:
    """Сначала то, что уже используют в атаках, потом по опасности и вероятности атаки."""
    d = _d(a)
    return (not d.get("kev"), _SEV_ORDER.get(d.get("severity", "unknown"), 5), -(d.get("epss") or 0),
            -(d.get("cvss") or 0), a.value)


@dataclass
class Url:
    asset: Asset
    scheme: str
    host: str
    port: int

    @property
    def data(self) -> dict[str, Any]:
        return _d(self.asset)


class Facts:
    """Находки цели, разложенные по полочкам. Строится один раз на страницу.

    ran — какие модули по цели уже отработали (id → когда). None — неизвестно: считаем, что все.
    """

    def __init__(self, target, rows: list[Asset], ran: dict[str, datetime] | None = None) -> None:
        self.target = target
        self.root: str = target.domain  # основной домен (или IP) цели
        self.rows = rows
        self.ran = ran
        self.by: dict[str, list[Asset]] = {}
        for a in rows:
            self.by.setdefault(a.kind, []).append(a)
        every = sorted(self.kind("ip"), key=lambda a: _ip_key(a.value))
        self.ips: dict[str, Asset] = {a.value: a for a in every if not _d(a).get("gone")}
        self.gone: dict[str, Asset] = {a.value: a for a in every if _d(a).get("gone")}
        self.names: dict[str, list[str]] = {}  # имя → его адреса
        for ip, a in self.ips.items():
            for name in _d(a).get("domains") or []:
                self.names.setdefault(name, []).append(ip)
        self.ports: dict[str, dict[int, Asset]] = {}
        for a in self.kind("port"):
            ip, port = _d(a).get("ip"), _d(a).get("port")
            if ip and port and ip in self.ips:
                self.ports.setdefault(ip, {})[int(port)] = a
        self.urls: list[Url] = []
        for a in self.kind("url"):
            u = urlsplit(a.value)
            if u.hostname:
                self.urls.append(Url(a, u.scheme, u.hostname, u.port or (443 if u.scheme == "https" else 80)))
        self.certs = {a.value.rsplit(":", 1)[0]: a for a in self.kind("certificate")}
        self.records: dict[str, list[Asset]] = {}
        for a in self.kind("dns_record"):
            self.records.setdefault((_d(a).get("type") or a.value.split()[0]).upper(), []).append(a)
        # программы (без исчезнувших) и уязвимости (без закрытых), уязвимости — по программе
        self.techs: dict[str, Asset] = {a.value: a for a in self.kind("technology") if _d(a).get("state") != "gone"}
        self.vulns: list[Asset] = sorted((a for a in self.kind("vulnerability") if _d(a).get("state") != "closed"),
                                         key=_vuln_rank)
        self.vulns_of: dict[str, list[Asset]] = {}
        for a in self.vulns:
            self.vulns_of.setdefault(_d(a).get("tech", ""), []).append(a)

    def kind(self, k: str) -> list[Asset]:
        return self.by.get(k, [])

    def tech_spots(self, tech: Asset) -> set[tuple[str, int]]:
        """Где работает программа: (IP, порт). Из «127.0.0.2:22» и «https://example.com» (IP — по имени)."""
        spots = set()
        for w in _d(tech).get("where") or []:
            if "://" in w:
                u = urlsplit(w)
                port = u.port or (443 if u.scheme == "https" else 80)
                for ip in self.names.get(u.hostname or "", []):
                    spots.add((ip, port))
            elif ":" in w:
                host, _, port = w.rpartition(":")
                if port.isdigit():
                    spots.add((host.strip("[]"), int(port)))
        return spots

    def did(self, module_id: str) -> bool:
        return self.ran is None or module_id in self.ran

    def did_cve(self) -> bool:
        """Уязвимости сверяли по-настоящему (а не «неизвестно, значит считаем, что да»)."""
        return self.ran is not None and "cve" in self.ran

    def record(self, rtype: str) -> dict[str, Any]:
        rows = self.records.get(rtype) or []
        return _d(rows[0]) if rows else {}

    @property
    def updated(self):
        return max((a.last_seen for a in self.rows if a.last_seen is not None), default=None)

    def name_rank(self, name: str) -> tuple:
        root = self.root
        return (name != root, name != f"www.{root}", name.count("."), name)

    def best(self, urls: list[Url]) -> Url | None:
        """Главная страница: основной домен, потом www, потом остальное."""
        return min(urls, key=lambda u: self.name_rank(u.host), default=None)

    def site(self) -> tuple[Url | None, Url | None]:
        """(https-страница, http-ответ) для самого главного имени цели."""
        https = self.best([u for u in self.urls if u.scheme == "https"])
        http = self.best([u for u in self.urls if u.scheme == "http"])
        return https, http

    def main_cert(self) -> dict[str, Any]:
        host = min(self.certs, key=self.name_rank, default=None)
        return _d(self.certs[host]) if host else {}

    def web_on(self, ip: str) -> dict[int, list[Url]]:
        out: dict[int, list[Url]] = {}
        for u in self.urls:
            if ip in self.names.get(u.host, []):
                out.setdefault(u.port, []).append(u)
        return out

    def node_of(self, host: str, port: int | None = None) -> str:
        """Якорь строки дерева «адрес имени → порт». Без порта — порт https этого имени."""
        if port is None:
            port = next((u.port for u in self.urls if u.scheme == "https" and u.host == host), 443)
        ips = self.names.get(host) or []
        return "#" + anchor(ips[0], port) if ips else "#tree"


def load(db, target) -> Facts:
    """Находки цели; в пределах одной сессии БД — один запрос на все блоки страницы."""
    key = ("recon.facts", target.id)
    if key not in db.info:
        rows = list(db.scalars(select(Asset).where(Asset.target_id == target.id).order_by(Asset.id)))
        ran = dict(db.execute(select(Run.module_id, func.max(Run.finished_at))
                              .where(Run.target_id == target.id, Run.status == "done").group_by(Run.module_id)).all())
        db.info[key] = Facts(target, rows, ran)
    return db.info[key]


def facts_for(db, targets) -> dict[int, Facts]:
    """Факты по многим целям тремя запросами, а не тремя на каждую (список целей, обзор)."""
    ids = [t.id for t in targets]
    if not ids:
        return {}
    by: dict[int, list[Asset]] = {}
    for a in db.scalars(select(Asset).where(Asset.target_id.in_(ids)).order_by(Asset.id)):
        by.setdefault(a.target_id, []).append(a)
    ran: dict[int, dict[str, datetime]] = {}
    for tid, mid, when in db.execute(select(Run.target_id, Run.module_id, func.max(Run.finished_at))
                                     .where(Run.target_id.in_(ids), Run.status == "done")
                                     .group_by(Run.target_id, Run.module_id)):
        ran.setdefault(tid, {})[mid] = when
    return {t.id: Facts(t, by.get(t.id, []), ran.get(t.id, {})) for t in targets}


def counts(found: list["Issue"]) -> dict[str, int]:
    return {level: sum(1 for i in found if i.level == level) for level in ("err", "warn", "info")}


# --- сводка ------------------------------------------------------------------------

@dataclass
class Fact:
    key: str
    value: list[Seg]
    sub: str = ""
    href: str = ""
    run: tuple[str, str] | None = None  # модуль ещё не запускался: (id, название) — вместо значения кнопка


@dataclass
class Group:
    title: str
    facts: list[Fact]


def _todo(key: str, module_id: str) -> Fact:
    return Fact(key, [], run=(module_id, TITLES.get(module_id, module_id)))


def summary(f: Facts) -> list[Group]:
    return [g for g in (_net(f), _site(f), _dns(f), _outside(f), _software(f)) if g.facts]


def upgrade_text(d: dict[str, Any]) -> str:
    """«8.0.43» или «версии новее 8.0.44» — когда база говорит только «уязвимы до 8.0.44 включительно»."""
    v = d.get("upgrade_to") or ""
    return f"версии новее {v}" if v and d.get("upgrade_after") else v


SEV_SHORT = {"critical": "крит.", "high": "выс.", "medium": "сред.", "low": "низ.", "unknown": "без оценки"}
SEV_TONE = {"critical": "err", "high": "err", "medium": "warn", "low": "dim", "unknown": "dim"}


def _software(f: Facts) -> Group:
    g = Group("программы и уязвимости", [])
    if not f.did("tech"):
        g.facts.append(_todo("программы", "tech"))
        return g
    techs = list(f.techs.values())
    versioned = [a for a in techs if _d(a).get("version")]
    shown = versioned or techs
    g.facts.append(Fact("программы", ([(a.value, "") for a in shown[:4]] + ([(f"+{len(shown) - 4}", "dim")] if len(shown) > 4 else []))
                        or [("не узнали", "dim")], sub=f"всего {len(techs)}, с версией {len(versioned)}" if techs else "",
                        href="#vulns" if f.did("cve") else "#found"))
    if not f.did("cve"):
        g.facts.append(_todo("уязвимости", "cve"))
        return g
    c = Counter(_d(a).get("severity") for a in f.vulns)
    segs = [(f"{SEV_SHORT[s]} {c[s]}", SEV_TONE[s]) for s in SEV_SHORT if c[s]]
    kev = sum(1 for a in f.vulns if _d(a).get("kev"))
    exploit = sum(1 for a in f.vulns if _d(a).get("exploit"))
    g.facts.append(Fact("уязвимости", segs or [("известных нет", "ok")], href="#vulns",
                        sub=", ".join(x for x in (f"в атаках {kev}" if kev else "", f"с эксплойтом {exploit}" if exploit else "",
                                                  f"всего {len(f.vulns)}" if f.vulns else "") if x)))
    if f.did("eol"):
        checked = [a for a in techs if _d(a).get("eol_checked") and _d(a).get("eol_cycle")]
        ended = [a for a in checked if _d(a).get("eol_state") == "ended"]
        soon = [a for a in checked if _d(a).get("eol_state") == "soon"]
        behind = [a for a in checked if _d(a).get("eol_behind")]
        segs = ([(f"{_d(a).get('title') or a.value} {_d(a).get('eol_cycle')}", "err") for a in ended[:3]]
                + ([(f"+{len(ended) - 3}", "dim")] if len(ended) > 3 else []))
        g.facts.append(Fact("поддержка", segs or ([("все ветки поддерживаются", "ok")] if checked else [("нет данных", "dim")]),
                            href="#issues", sub=", ".join(x for x in (
                                f"без поддержки {len(ended)}" if ended else "", f"скоро закончится {len(soon)}" if soon else "",
                                f"отстают внутри ветки {len(behind)}" if behind else "") if x)))
    upgrades = [(_d(a).get("title") or a.value, upgrade_text(_d(a)).replace("версии ", ""))
                for a in techs if _d(a).get("upgrade_to")]
    if upgrades:
        g.facts.append(Fact("обновить", [(f"{t} → {v}", "warn") for t, v in upgrades[:3]]
                            + ([(f"+{len(upgrades) - 3}", "dim")] if len(upgrades) > 3 else []), href="#vulns"))
    return g


def _net(f: Facts) -> Group:
    g = Group("сеть", [])
    if not f.did("resolve"):
        g.facts.append(_todo("адреса", "resolve"))
        return g
    if not f.ips:
        g.facts.append(Fact("адреса", [("имена не разрешаются", "err")]))
        return g
    addrs = list(f.ips)
    g.facts.append(Fact("адреса", [(ip, "") for ip in addrs[:3]] + ([(f"+{len(addrs) - 3}", "dim")] if len(addrs) > 3 else []),
                        sub=f"больше не используются: {', '.join(f.gone)}" if f.gone else "", href="#tree"))
    if not f.did("asn"):
        g.facts.append(_todo("хостинг", "asn"))
    else:
        own = [a for a in f.ips.values() if not _d(a).get("cdn")]
        cdn = sorted({_d(a)["cdn"] for a in f.ips.values() if _d(a).get("cdn")})
        host = next((a for a in own if _d(a).get("asn")), None)
        if host:
            d = _d(host)
            g.facts.append(Fact("хостинг", [(netkit.org_name(d.get("as_name")) or "?", ""), (f"AS{d['asn']}", "dim"),
                                            (d.get("country") or "", "dim")], sub=d.get("prefix") or ""))
        elif own and all(_d(a).get("private") for a in own):
            g.facts.append(Fact("хостинг", [("частная сеть", "dim")], sub="адрес не из интернета"))
        elif cdn and not own:
            g.facts.append(Fact("хостинг", [("не виден", "warn")], sub="сайт за CDN — настоящий сервер скрыт"))
        else:
            g.facts.append(Fact("хостинг", [("не определился", "dim")]))
        g.facts.append(Fact("cdn", [(", ".join(cdn), "warn")] if cdn else [("нет", "")],
                            sub="" if cdn else "сервер виден напрямую"))
    if not f.did("ptr"):
        g.facts.append(_todo("имя", "ptr"))
    else:
        ptrs = [_d(a).get("ptr") for a in f.ips.values() if _d(a).get("ptr")]
        g.facts.append(Fact("имя", [(ptrs[0], "")] + ([(f"+{len(ptrs) - 1}", "dim")] if len(ptrs) > 1 else [])
                            if ptrs else [("без обратного имени", "dim")]))
    if not f.did("ping"):
        g.facts.append(_todo("пинг", "ping"))
    else:
        rtts = [_d(a)["rtt_ms"] for a in f.ips.values() if _d(a).get("rtt_ms") is not None]
        dead = [ip for ip, a in f.ips.items() if _d(a).get("reachable") is False]
        value = [(netkit.fmt_ms(min(rtts)), "")] if rtts else []
        if dead:
            value.append((f"не отвечает {len(dead)}" if rtts else "не отвечает", "err"))
        g.facts.append(Fact("пинг", value or [("—", "dim")]))
    return g


def _site(f: Facts) -> Group:
    g = Group("сайт", [])
    https, http = f.site()
    if not f.did("http"):
        g.facts.append(_todo("ответ", "http"))
    elif https is None and http is None:
        g.facts.append(Fact("ответ", [("не отвечает", "err")], sub="ни http, ни https"))
    else:
        page = https or (http if not http.data.get("to_https") else None)
        if page is not None:
            d = page.data
            g.facts.append(Fact("ответ", [(str(d.get("status")), _status_tone(d.get("status"))),
                                          (d.get("server") or "", "dim"),
                                          (netkit.fmt_ms(d["ms"]) if d.get("ms") is not None else "", "dim")],
                                sub=page.asset.value, href=f.node_of(page.host, page.port)))
            if d.get("title"):
                g.facts.append(Fact("страница", [(f"«{d['title']}»", "")]))
        g.facts.append(Fact("https", [("есть", "ok")] if https else [("нет", "err")],
                            sub="" if https else "сайт открывается только без шифрования"))
        if http is not None:
            to = http.data.get("to_https")
            g.facts.append(Fact("с http", [("уводит на https", "ok")] if to else [("остаётся на http", "warn")]))
        if https is not None:
            d = https.data
            g.facts.append(Fact("hsts", [("есть", "ok")] if d.get("hsts") else [("нет", "dim")]))
            missing = d.get("missing") or []
            g.facts.append(Fact("заголовки", [("все на месте", "ok")] if not missing else
                                [(f"нет {len(missing)} из 4", "warn" if len(missing) > 2 else "dim")],
                                sub=", ".join(missing), href="#issues" if missing else ""))
            cookies = d.get("cookies") or []
            if cookies:
                bad = [c["name"] for c in cookies if not c.get("secure")]
                g.facts.append(Fact("cookie", [(f"без Secure: {', '.join(bad)}", "warn")] if bad else
                                    [(f"{len(cookies)} с флагом Secure", "ok")]))
    if not f.did("tls"):
        g.facts.append(_todo("сертификат", "tls"))
    else:
        c = f.main_cert()
        tech = "  ".join(x for x in (c.get("protocol"), c.get("cipher")) if x)
        if not c:
            g.facts.append(Fact("сертификат", [("не получен", "dim")]))
        elif c.get("failed"):
            g.facts.append(Fact("сертификат", [("https не отвечает", "err")], sub=c["failed"]))
        elif c.get("valid") is False:
            g.facts.append(Fact("сертификат", [(c.get("error") or "не проходит проверку", "err")], sub=tech,
                                href="#issues"))
        else:
            days = c.get("days_left")
            g.facts.append(Fact("сертификат", [(c.get("issuer") or "?", ""), (f"до {c.get('expires')}", "dim"),
                                               (f"{days} дн", _days_tone(days) or "ok")],
                                sub=tech + (f"  имён в нём {len(c.get('sans') or [])}" if c.get("sans") else "")))
    return g


def _spf_seg(f: Facts) -> list[Seg]:
    spf = f.record("SPF")
    records = spf.get("records") or []
    if len(records) > 1:
        return [(f"записей {len(records)} — не работает", "err")]
    if not records:
        return [("нет", "warn")]
    policy = spf.get("all") or ""
    tone = {"-all": "ok", "~all": "ok", "?all": "warn", "+all": "err"}.get(policy, "")
    return [(policy or "без all", tone)]


def _dns(f: Facts) -> Group:
    g = Group("dns и почта", [])
    if _is_ip(f.root):  # у IP-адреса нет ни зоны, ни почты
        return g
    if not f.did("zone"):
        g.facts.append(_todo("dns", "zone"))
    else:
        ns = f.records.get("NS") or []
        if not ns:
            g.facts.append(Fact("dns", [("NS не найдены", "warn")]))
        else:
            by = Counter(_d(a).get("provider") for a in ns if _d(a).get("provider")).most_common(1)
            g.facts.append(Fact("dns", [(by[0][0] if by else "свои серверы", "")],
                                sub="  ".join(sorted(_d(a).get("host") or "" for a in ns))))
            sec = f.record("DNSSEC")
            g.facts.append(Fact("dnssec", [("подписана", "ok")] if sec.get("signed") else [("нет", "dim")]))
            caa = f.record("CAA")
            if caa:
                g.facts.append(Fact("caa", [(", ".join(caa.get("issuers") or []) or "запрещено всем", "ok")]
                                    if caa.get("records") else [("нет", "dim")],
                                    sub="" if caa.get("records") else "сертификат может выпустить любой центр"))
    if not f.did("mail"):
        g.facts.append(_todo("почта", "mail"))
    else:
        mx = sorted(f.records.get("MX") or [], key=lambda a: int(_d(a).get("priority") or 99))
        if mx:
            provider = next((_d(a).get("provider") for a in mx if _d(a).get("provider")), None)
            g.facts.append(Fact("почта", [(provider or "свой сервер", "")], sub="  ".join(_d(a).get("host") or "" for a in mx)))
        else:
            g.facts.append(Fact("почта", [("не принимается", "dim")], sub="нет MX-записей"))
        g.facts.append(Fact("spf", _spf_seg(f), href="#issues"))
        dmarc = f.record("DMARC")
        policy = dmarc.get("policy") or ""
        g.facts.append(Fact("dmarc", [(f"p={policy}", "ok" if policy in ("reject", "quarantine") else "warn")]
                            if dmarc.get("records") else [("нет", "warn")], href="#issues"))
    return g


def _is_ip(value: str) -> bool:
    try:
        ipaddress.ip_address(value)
        return True
    except ValueError:
        return False


def _outside(f: Facts) -> Group:
    g = Group("снаружи", [])
    if not f.did("ports"):
        g.facts.append(_todo("порты", "ports"))
    else:
        ports = [a for per in f.ports.values() for a in per.values()]
        opened = [a for a in ports if _d(a).get("state") != "closed"]
        nums: dict[int, bool] = {}
        for a in opened:
            nums[int(_d(a)["port"])] = nums.get(int(_d(a)["port"]), False) or bool(_d(a).get("risky"))
        hosts = len({_d(a)["ip"] for a in opened})
        g.facts.append(Fact("порты", [(str(p), "err" if bad else "") for p, bad in sorted(nums.items())]
                            or [("открытых не нашлось", "dim")],
                            sub=f"открыто {len(opened)} на {hosts} {plural(hosts, 'адресе', 'адресах', 'адресах')}"
                            if opened else "", href="#tree"))
        risky = sorted({(int(_d(a)["port"]), _d(a).get("service") or RISKY.get(int(_d(a)["port"]), "")) for a in opened
                        if _d(a).get("risky")})
        g.facts.append(Fact("опасные", [(f"{p} {s}".strip(), "err") for p, s in risky] or [("нет", "ok")],
                            href="#issues" if risky else ""))
        closed = sorted({int(_d(a)["port"]) for a in ports if _d(a).get("state") == "closed"})
        if closed:
            g.facts.append(Fact("закрылись", [(str(p), "dim") for p in closed], sub="были открыты в прошлый раз"))
    if not f.did("neighbors"):
        g.facts.append(_todo("соседи", "neighbors"))
    else:
        nb = f.kind("neighbor")
        rel = sum(1 for a in nb if _d(a).get("related"))
        g.facts.append(Fact("соседи", ([(f"{len(nb)} с именами", "")] if nb else [("с именами не нашлось", "dim")])
                            + ([(f"{rel} похож на ваш", "warn")] if rel else []), href="#tree" if nb else ""))
    return g


# --- замечания ----------------------------------------------------------------------

@dataclass
class Issue:
    level: str  # err | warn | info
    title: str
    where: str
    hint: str
    href: str = ""
    order: tuple = (0,)  # порядок внутри уровня: уязвимые программы — после остального, опаснее — выше


# Что значит открытый наружу сервис и что с ним делать — по названию сервиса, а не по номеру порта:
# MySQL на 3307 так же опасен, как на 3306.
RISK_HINTS = (
    ({"mysql", "postgresql", "mssql", "oracle", "mongodb", "redis", "elasticsearch", "couchdb", "influxdb", "memcached"},
     "База данных принимает подключения из интернета: пароль подбирают ботами, а без пароля данные читаются сразу. "
     "Пусть слушает только 127.0.0.1, или закройте порт файрволом."),
    ({"rdp", "vnc"}, "Удалённый рабочий стол виден всем — его постоянно перебирают. Спрячьте за VPN."),
    ({"ftp", "telnet"}, "Логин и пароль идут открытым текстом. Замените на SFTP или SSH, а порт закройте."),
    ({"docker", "docker-tls", "kubernetes", "kubelet"},
     "Управление контейнерами снаружи — это полный доступ к серверу. Закрыть немедленно."),
    ({"smb", "netbios", "nfs", "rsync"}, "Общие папки видны из интернета. Закройте порт файрволом."),
)


def risk_hint(service: str) -> str:
    name = (service or "").lower()
    for services, hint in RISK_HINTS:
        if name in services:
            return hint
    return "Панель управления открыта всем. Ограничьте доступ по IP или спрячьте за VPN."


def issues(f: Facts) -> list[Issue]:
    out: list[Issue] = []
    _ports_issues(f, out)
    _web_issues(f, out)
    _cert_issues(f, out)
    _mail_issues(f, out)
    _zone_issues(f, out)
    _vuln_issues(f, out)
    _support_issues(f, out)
    # одно и то же на нескольких именах (общий сертификат example.com и www) — одной строкой
    merged: dict[tuple[str, str, str], Issue] = {}
    for i in out:
        key = (i.level, i.title, i.hint)
        if key in merged:
            if i.where not in merged[key].where.split("  "):
                merged[key].where += "  " + i.where
        else:
            merged[key] = i
    rank = {"err": 0, "warn": 1, "info": 2}
    return sorted(merged.values(), key=lambda i: (rank[i.level], i.order, i.where, i.title))


def _ports_issues(f: Facts, out: list[Issue]) -> None:
    for ip, per in f.ports.items():
        for port, a in per.items():
            d = _d(a)
            if d.get("risky") and d.get("state") != "closed":
                service = d.get("service") or RISKY.get(port, "")
                what = f" — {d['banner']}" if d.get("banner") else ""
                out.append(Issue("err", f"{port} {service} открыт в интернет", ip, risk_hint(service) + what,
                                 "#" + anchor(ip, port)))
            elif d.get("state") == "closed":
                out.append(Issue("info", f"{port} {d.get('service') or ''} закрылся".replace("  ", " "), ip,
                                 "В прошлый раз был открыт. Если закрывали намеренно — всё хорошо.", "#" + anchor(ip, port)))
    for ip, a in f.ips.items():
        if _d(a).get("reachable") is False:
            out.append(Issue("warn", "адрес не отвечает", ip, "Ни 443, ни 80 не приняли подключение: сервер выключен "
                             "или закрыт файрволом.", "#" + anchor(ip)))


def _web_issues(f: Facts, out: list[Issue]) -> None:
    https, http = f.site()
    if http is not None and not http.data.get("to_https"):
        out.append(Issue("warn", "http:// не уводит на https://", http.host,
                         "Кто набрал адрес без https, остаётся на незашифрованном соединении: пароли и формы видны по пути.",
                         f.node_of(http.host, http.port)))
    if https is None and http is not None:
        out.append(Issue("err", "https не работает", http.host, "Сайт открывается только без шифрования.",
                         f.node_of(http.host, http.port)))
    for u in f.urls:
        if u.scheme == "https" and (u.data.get("status") or 0) >= 500:
            out.append(Issue("err", f"сайт отвечает ошибкой {u.data['status']}", u.host,
                             "Сервер сломан или перегружен.", f.node_of(u.host, u.port)))
    if https is None:
        return
    d, at = https.data, f.node_of(https.host, https.port)
    if not d.get("hsts"):
        out.append(Issue("info", "нет HSTS", https.host, "Браузер не запоминает, что сайт только на https, — "
                         "первый заход могут перехватить. Заголовок Strict-Transport-Security.", at))
    if d.get("missing"):
        out.append(Issue("info", f"нет защитных заголовков: {', '.join(d['missing'])}", https.host,
                         "CSP мешает внедрить чужой скрипт, X-Frame-Options — встроить сайт в чужую страницу "
                         "(кликджекинг), X-Content-Type-Options — подсунуть файл другого типа, Referrer-Policy — "
                         "утечке адресов страниц.", at))
    insecure = [c["name"] for c in d.get("cookies") or [] if not c.get("secure")]
    if insecure:
        out.append(Issue("warn", f"cookie без флага Secure: {', '.join(insecure)}", https.host,
                         "Такая cookie может уйти по http и попасть к перехватчику. Ставьте Secure, а сессионным "
                         "ещё HttpOnly и SameSite.", at))
    if d.get("powered_by"):
        out.append(Issue("info", f"сервер называет движок: {d['powered_by']}", https.host,
                         "По версии ищут известные уязвимости. Заголовок X-Powered-By лучше убрать.", at))
    if re.search(r"/\d", d.get("server") or ""):
        out.append(Issue("info", f"видна версия сервера: {d['server']}", https.host,
                         "По версии ищут известные уязвимости. В nginx — server_tokens off.", at))


def _cert_issues(f: Facts, out: list[Issue]) -> None:
    for host, c in f.certs.items():
        d = _d(c)
        if d.get("valid") is False:
            out.append(Issue("err", "сертификат не проходит проверку", host,
                             f"{d.get('error') or ''}. Браузер покажет посетителю «Подключение не защищено».".lstrip(". "),
                             f.node_of(host)))
        elif d.get("days_left") is not None and d["days_left"] < 21:
            out.append(Issue("err" if d["days_left"] < 7 else "warn", f"сертификат кончается через {d['days_left']} дн",
                             host, f"Действует до {d.get('expires')} ({d.get('issuer')}). Не продлят — сайт перестанет "
                             "открываться без предупреждения.", f.node_of(host)))
        if d.get("protocol") in ("TLSv1", "TLSv1.1"):
            out.append(Issue("warn", f"устаревший протокол {d['protocol']}", host,
                             "Браузеры его уже не поддерживают, а шифрование в нём слабое. Включите TLS 1.2 и 1.3.",
                             f.node_of(host)))


def _mail_issues(f: Facts, out: list[Issue]) -> None:
    if not f.did("mail") or not f.records.get("SPF"):
        return
    zone = f.record("SPF").get("zone") or f.root
    has_mx = bool(f.records.get("MX"))
    spf = f.record("SPF")
    records = spf.get("records") or []
    if len(records) > 1:
        out.append(Issue("err", f"записей SPF {len(records)} — не работает ни одна", zone,
                         "По стандарту запись SPF должна быть одна, иначе проверка падает с ошибкой. Объедините их.",
                         "#" + anchor("dns")))
    elif not records:
        if has_mx:
            out.append(Issue("warn", "нет SPF", zone, "Письма от имени домена может отправить кто угодно: "
                             "почтовые сервисы не отличат подделку. Добавьте TXT-запись v=spf1 с серверами, "
                             "которые реально шлют вашу почту.", "#" + anchor("dns")))
        else:
            out.append(Issue("info", "домен не шлёт почту, но подделку не запрещает", zone,
                             "Чтобы письма от его имени не принимали, добавьте TXT v=spf1 -all и DMARC p=reject.",
                             "#" + anchor("dns")))
    elif spf.get("all") == "+all":
        out.append(Issue("err", "SPF разрешает отправлять всем (+all)", zone,
                         "Это то же, что не иметь SPF: подделку не отличить. Замените на -all или ~all.", "#" + anchor("dns")))
    elif spf.get("all") == "?all":
        out.append(Issue("warn", "SPF ничего не запрещает (?all)", zone,
                         "Почтовые сервисы не отбрасывают чужие письма от вашего имени. Замените на ~all или -all.",
                         "#" + anchor("dns")))
    dmarc = f.record("DMARC")
    if not dmarc.get("records"):
        if has_mx or records:
            out.append(Issue("warn", "нет DMARC", zone, "Почтовые сервисы не знают, что делать с поддельными письмами "
                             f"от вашего имени. Добавьте TXT _dmarc.{zone}: v=DMARC1; p=quarantine; rua=mailto:…",
                             "#" + anchor("dns")))
    elif dmarc.get("policy") == "none":
        out.append(Issue("info", "DMARC только наблюдает (p=none)", zone,
                         "Поддельные письма всё ещё доходят. Когда отчёты чистые — переходите на p=quarantine или reject.",
                         "#" + anchor("dns")))


def _zone_issues(f: Facts, out: list[Issue]) -> None:
    caa = f.record("CAA")
    if caa and not caa.get("records"):
        out.append(Issue("info", "нет CAA", caa.get("zone") or f.root,
                         "Сертификат на домен может выпустить любой удостоверяющий центр. Запись CAA ограничит их "
                         "теми, кем вы пользуетесь (например, letsencrypt.org).", "#" + anchor("dns")))


def _support_issues(f: Facts, out: list[Issue]) -> None:
    """Поддержка ветки. У уязвимой программы — в том же замечании (одна программа — одно замечание),
    у программы без известных CVE — своим: ветка без поддержки и так открытая дверь на будущее."""
    by_href = {i.href: i for i in out}
    for value, tech in f.techs.items():
        d = _d(tech)
        state, cycle = d.get("eol_state"), d.get("eol_cycle")
        if not d.get("eol_checked") or not cycle:
            continue
        where = ", ".join((d.get("where") or [])[:2])
        href = "#" + anchor("vulns", d.get("product"), d.get("version"))
        newest = d.get("eol_newest")
        move = (f" Переходите на ветку {newest}" + (f" (сейчас {d['eol_newest_latest']})" if d.get("eol_newest_latest") else "")
                + "." if newest and newest != cycle else "")
        inside = f" Последний выпуск ветки {cycle} — {d['eol_latest']}." if d.get("eol_behind") else ""
        same = by_href.get(href)
        if same is not None:  # уже есть замечание об уязвимостях этой программы — дополняем его
            when = d.get("eol_date")
            if state == "ended":
                same.title += f"; ветка {cycle} без поддержки" + (f" с {when}" if when else "")
                hint = same.hint
                up, latest = d.get("upgrade_to"), d.get("eol_newest_latest")
                if move and up and latest and versions.compare(latest, up) >= 0:
                    # переход на актуальную ветку закрывает и известные дыры — один совет вместо двух
                    hint = hint.replace(f" Обновите до {up} или новее.", "").replace(f" Обновите до версии новее {up}.", "")
                    move = move[:-1] + " — это закроет и известные уязвимости."
                same.hint = "Ветку больше не чинят: новые дыры останутся открытыми навсегда." + move + " " + hint + inside
                same.level = "err"
            elif state == "soon":
                same.title += f"; поддержка ветки {cycle} до {when}"
                same.hint += f" Поддержка ветки {cycle} закончится через {d.get('eol_days')} дн." + move
            elif d.get("eol_behind"):
                same.hint += f" Внутри ветки {cycle} уже есть {d['eol_latest']} — это обновление можно ставить сразу."
            continue
        if state == "ended":
            when = d.get("eol_date")
            out.append(Issue("err", f"{value}: ветка {cycle} без поддержки" + (f" с {when}" if when else ""), where,
                             "Исправлений безопасности для неё больше не выпускают: новые дыры останутся открытыми "
                             "навсегда." + move + inside, href, (2,)))
        elif state == "soon":
            out.append(Issue("warn", f"{value}: поддержка ветки {cycle} закончится {d.get('eol_date')}", where,
                             f"Осталось {d.get('eol_days')} дн. Потом исправлений не будет." + move + inside, href, (2,)))
        elif d.get("eol_behind"):
            out.append(Issue("warn", f"{value}: в ветке {cycle} уже есть {d.get('eol_latest')}", where,
                             "Обновление внутри ветки — только исправления, без смены поведения: его можно ставить сразу.",
                             href, (3,)))


def _vuln_issues(f: Facts, out: list[Issue]) -> None:
    """Уязвимая программа — одно замечание: сколько и каких, самые опасные и до какой версии обновиться."""
    for value, tech in f.techs.items():
        d = _d(tech)
        if d.get("closed"):
            out.append(Issue("warn", f"{value}: убран из каталога WordPress", ", ".join((d.get("where") or [])[:2]),
                             f"Причина: {d['closed']}. Обновлений и исправлений больше не будет — замените его "
                             "живым аналогом.", "#" + (anchor("vulns", d.get("product"), d.get("version"))
                                                         if f.vulns_of.get(value) else "found")))
        vs = f.vulns_of.get(value) or []
        if not vs:
            continue
        c = Counter(_d(a).get("severity") for a in vs)
        kev = sum(1 for a in vs if _d(a).get("kev"))
        level = "err" if kev or c["critical"] or c["high"] else "warn" if c["medium"] else "info"
        parts = [f"{SEV_SHORT[s]} {c[s]}" for s in SEV_SHORT if c[s]]
        title = (f"{value}: уязвимостей {len(vs)}" + (f" ({', '.join(parts)})" if parts else "")
                 + (f", в атаках {kev}" if kev else ""))
        worst = "; ".join(f"{a.value} ({_d(a)['cvss'] if _d(a).get('cvss') is not None else '—'}"
                          f"{', в атаках' if _d(a).get('kev') else ', есть эксплойт' if _d(a).get('exploit') else ''})"
                          for a in vs[:3])
        hint = f"Самые опасные: {worst}. " + (
            f"Обновите до версии новее {d['upgrade_to']}." if d.get("upgrade_to") and d.get("upgrade_after")
            else f"Обновите до {d['upgrade_to']} или новее." if d.get("upgrade_to") else "Обновите до актуальной версии.")
        if d.get("no_fix"):
            hint += (f" У {d['no_fix']} из {len(vs)} версия с исправлением не названа — проверьте у разработчика "
                     "или замените программу.")
        if d.get("distro"):
            hint += (f" Это сборка {d['distro']}: часть исправлений могла прийти обновлением пакета без смены "
                     f"версии — сверьте с бюллетенями {d['distro']}.")
        out.append(Issue(level, title, ", ".join((d.get("where") or [])[:2]), hint,
                         "#" + anchor("vulns", d.get("product"), d.get("version")),
                         (1, -kev, -c["critical"], -c["high"], -c["medium"], -len(vs))))


NEW_DAYS = 7  # «новая»: появилась с прошлой проверки не раньше недели назад


def support_tag(d: dict[str, Any]) -> Seg | None:
    """Поддержка ветки одной меткой: «без поддержки с 2025-12-31», «поддержка до 2027-03-31»…"""
    state, cycle, when = d.get("eol_state"), d.get("eol_cycle"), d.get("eol_date")
    if not d.get("eol_checked") or state in (None, "unknown"):
        return None
    if state == "ended":
        return (f"ветка {cycle} без поддержки" + (f" с {when}" if when else ""), "err")
    if state == "soon":
        return (f"ветка {cycle}: поддержка до {when}", "warn")
    if d.get("eol_behind"):
        return (f"в ветке {cycle} есть {d.get('eol_latest')}", "warn")
    return (f"ветка {cycle} поддерживается" + (f" до {when}" if when else ""), "dim")


def _is_new(v: dict[str, Any]) -> bool:
    when = v.get("new_at")
    if not when:
        return False
    try:
        return (utcnow() - datetime.fromisoformat(when)).days < NEW_DAYS
    except ValueError:
        return False


def vuln_groups(f: Facts, limit: int = 200) -> list[dict[str, Any]]:
    """Раздел «Уязвимости»: по программе — счётчики, где работает, до чего обновить и сами CVE.
    Программа без известных CVE, но без поддержки (или с обновлением внутри ветки) — тоже здесь."""
    groups = []
    quiet = {value: [] for value, tech in f.techs.items() if value not in f.vulns_of and support_tag(_d(tech))
             and (_d(tech).get("eol_state") in ("ended", "soon") or _d(tech).get("eol_behind"))}
    for value, vs in [*f.vulns_of.items(), *quiet.items()]:
        tech = f.techs.get(value)
        d = _d(tech) if tech is not None else _d(vs[0])
        c = Counter(_d(a).get("severity") for a in vs)
        kev = sum(1 for a in vs if _d(a).get("kev"))
        rows = []
        for a in vs[:limit]:
            v = _d(a)
            flags = ([("новая", "accent")] if _is_new(v) else []) \
                + ([("в атаках", "err")] if v.get("kev") else []) + ([("вымогатели", "err")] if v.get("ransomware") else []) \
                + ([("эксплойт", "warn")] if v.get("exploit") else []) \
                + ([("нет исправления", "err")] if v.get("unfixed") else []) \
                + ([("все версии", "dim")] if v.get("how") == "all" else [])
            epss = v.get("epss")
            summary_text = v.get("summary") or ""
            link = v.get("link") or (f"https://nvd.nist.gov/vuln/detail/{a.value}" if a.value.startswith("CVE-") else "")
            link = link if link.startswith(("https://", "http://")) else ""  # ссылки пришли из чужой базы
            rows.append({"id": a.value, "link": link, "severity": v.get("severity", "unknown"),
                         "epss": (f"{epss * 100:.0f}%" if epss >= 0.005 else "<1%") if epss is not None else "",
                         "epss_tone": "" if epss is None else "err" if epss >= 0.5 else "warn" if epss >= 0.1 else "dim",
                         "score": f"{v['cvss']:.1f}" if v.get("cvss") is not None else "—", "flags": flags,
                         "fixed": v.get("fixed_in") or "", "published": v.get("published", ""),
                         "summary": summary_text,
                         "cwe": ", ".join(v.get("cwe") or [])})
        groups.append({
            "anchor": anchor("vulns", d.get("product"), d.get("version")),
            "title": d.get("title") or d.get("product_title") or value, "version": d.get("version", ""),
            "where": _first(d.get("where") or []), "where_all": ", ".join(d.get("where") or []),
            "distro": d.get("distro", ""), "package": d.get("package", ""),
            "closed": d.get("closed", "") if tech is not None else "",
            "counts": [(f"{SEV_SHORT[s]} {c[s]}", SEV_TONE[s]) for s in SEV_SHORT if c[s]]
            + ([(f"в атаках {kev}", "err")] if kev else []),
            "upgrade": upgrade_text(d), "total": len(vs), "more": max(0, len(vs) - limit), "rows": rows,
            "hot": bool(kev or c["critical"] or c["high"]), "support": support_tag(d),
            "fresh": sum(1 for a in vs if _is_new(_d(a))),
            "rank": (not kev, _SEV_ORDER.get(_d(vs[0]).get("severity", "unknown") if vs else "none", 5)
                     if vs else 6, -len(vs)),
        })
    return sorted(groups, key=lambda g: g["rank"])


def _first(items: list[str]) -> str:
    """«https://example.com +1»: первое и сколько ещё — чтобы строка группы помещалась в одну линию."""
    return (items[0] + (f" +{len(items) - 1}" if len(items) > 1 else "")) if items else ""


def vuln_totals(f: Facts) -> list[Seg]:
    """Строка над разделом: сколько всего и каких, сколько в атаках и с эксплойтом, у скольких программ."""
    c = Counter(_d(a).get("severity") for a in f.vulns)
    kev = sum(1 for a in f.vulns if _d(a).get("kev"))
    exploit = sum(1 for a in f.vulns if _d(a).get("exploit"))
    fresh = sum(1 for a in f.vulns if _is_new(_d(a)))
    ended = sum(1 for t in f.techs.values() if _d(t).get("eol_state") == "ended")
    return ([(f"новых {fresh}", "accent")] if fresh else []) + ([(f"{SEV_SHORT[s]} {c[s]}", SEV_TONE[s]) for s in SEV_SHORT if c[s]]
            + ([(f"в атаках {kev}", "err")] if kev else []) + ([(f"с эксплойтом {exploit}", "warn")] if exploit else [])
            + ([(f"без поддержки {ended}", "err")] if ended else []) + [(f"программ {len(f.vulns_of)}", "dim")])


def unchecked(f: Facts) -> list[str]:
    """Программы, у которых не видно точной версии: уязвимости по ним не проверить."""
    out = []
    for value, tech in f.techs.items():
        d = _d(tech)
        if d.get("cve_lookup") and d.get("precision") != "exact":
            out.append(value + ("" if d.get("version") else " (версия скрыта)"))
    return out


# --- структура ----------------------------------------------------------------------

@dataclass
class Node:
    items: list[tuple[str, str]]  # [(что скопировать, что показать)]; пустое «что скопировать» — просто текст
    kind: str = ""
    id: str = ""
    cols: list[Seg] = field(default_factory=list)  # выровненные колонки: сервис у порта, тип у записи
    meta: list[Seg] = field(default_factory=list)
    tags: list[Seg] = field(default_factory=list)
    children: list[Node] = field(default_factory=list)
    dim: bool = False
    href: str = ""


def tree(f: Facts) -> Node:
    t = f.target
    root = Node([(t.name, t.name)], "root", anchor("root"))
    seen: set[str] = set()

    groups: dict[tuple[str, ...], list[str]] = {}
    for name, ips in f.names.items():
        groups.setdefault(tuple(sorted(set(ips), key=_ip_key)), []).append(name)
    for ips, names in sorted(groups.items(), key=lambda kv: min(f.name_rank(n) for n in kv[1])):
        names.sort(key=f.name_rank)
        node = Node([(n, n) for n in names], "name", anchor(names[0]))
        # CNAME показываем, только если он ведёт наружу (www → example.com очевиден и так)
        cnames = [c for ip in ips for c in (_d(f.ips[ip]).get("cnames") or []) if c not in names]
        if cnames:
            node.meta.append((f"cname {cnames[-1]}", "dim"))
        node.children = [_ip_node(f, ip, seen) for ip in ips]
        root.children.append(node)

    for ip in f.ips:  # адреса, на которые не указывает ни одно имя: IP из хостов цели
        if ip not in seen:
            node = _ip_node(f, ip, seen)
            node.tags.insert(0, ("из хостов", "dim"))
            root.children.append(node)

    if f.gone:
        node = Node([("", "больше не используются")], "group", anchor("gone"), meta=[(str(len(f.gone)), "dim")])
        for ip, a in f.gone.items():
            node.children.append(Node([(ip, ip)], "ip", dim=True, meta=[("раньше: " + ", ".join(_d(a).get("was") or []), "dim")],
                                      tags=[("не трогаем", "dim")]))
        root.children.append(node)

    loose = sorted((a for a in f.kind("subdomain") + f.kind("domain") if a.value not in f.names),
                   key=lambda a: f.name_rank(a.value))
    if loose:
        node = Node([("", "ещё имена")], "group", anchor("names"), meta=[(str(len(loose)), "dim")])
        for a in loose:
            d = _d(a)
            src = f"из сертификата {d.get('seen_on')}" if d.get("source") == "сертификат" else (d.get("source") or "")
            child = Node([(a.value, a.value)], "name", meta=[(src, "dim")])
            if not t.scope.allows(a.value):
                child.tags.append(("не в цели", "dim"))
            node.children.append(child)
        root.children.append(node)

    dns = _dns_node(f)
    if dns.children:
        root.children.append(dns)
    return root


def _dns_node(f: Facts) -> Node:
    node = Node([("", "dns")], "group", anchor("dns"))
    order = {"NS": 0, "SOA": 1, "DNSSEC": 2, "CAA": 3, "MX": 4, "SPF": 5, "DMARC": 6}
    for typ in sorted(f.records, key=lambda x: (order.get(x, 9), x)):
        rows = f.records[typ]
        provider = Counter(_d(a).get("provider") for a in rows if _d(a).get("provider")).most_common(1)
        child = Node([("", typ.lower())], "rec", meta=[(provider[0][0], "")] if provider else [])
        if typ == "MX":
            rows = sorted(rows, key=lambda a: int(_d(a).get("priority") or 99))
            child.meta += [(_d(a).get("detail") or a.value, "dim") for a in rows]
        elif typ == "NS":
            child.meta += [(h, "dim") for h in sorted(_d(a).get("host") or a.value.split(" ", 1)[-1] for a in rows)]
        elif typ == "SPF":
            records = _d(rows[0]).get("records") or []
            segs = _spf_seg(f)
            child.meta += [(r, "dim") for r in records] or []
            child.tags += [s for s in segs if s[1] in ("err", "warn")]
            if not records:
                child.meta.append(("нет", "warn"))
        elif typ == "DMARC":
            d = _d(rows[0])
            child.meta.append((d.get("detail") or "нет", "dim" if d.get("records") else "warn"))
        else:
            child.meta += [(_d(a).get("detail") or a.value, "dim") for a in rows]
        node.children.append(child)
    return node


def _ip_node(f: Facts, ip: str, seen: set[str]) -> Node:
    node = Node([(ip, ip)], "ip", anchor(ip))
    if ip in seen:  # тот же адрес у другой группы имён: подробности уже выше
        return Node([(ip, ip)], "ip", meta=[("подробно выше", "dim")], href="#" + anchor(ip))
    seen.add(ip)
    d = _d(f.ips[ip])
    if d.get("private"):
        node.meta.append(("частная сеть", "dim"))
    elif d.get("asn"):
        node.meta += [(netkit.org_name(d.get("as_name")) or "?", ""), (f"AS{d['asn']}", "dim"), (d.get("country") or "", "dim")]
    if d.get("ptr"):
        node.meta.append((f"ptr {d['ptr']}", "dim"))
    if d.get("rtt_ms") is not None:
        node.meta.append((f"пинг {netkit.fmt_ms(d['rtt_ms'])}", "dim"))
    if d.get("reachable") is False:
        node.tags.append(("не отвечает", "err"))
    if d.get("cdn"):
        node.tags.append((d["cdn"], "warn"))

    ports = f.ports.get(ip, {})
    web = f.web_on(ip)
    for port in sorted(set(ports) | set(web)):
        node.children.append(_port_node(f, ip, port, ports.get(port), web.get(port, [])))
    if d.get("cdn") and not ports:
        node.children.append(Node([("", "порты CDN не сканируются — это не сервер владельца")], "note", dim=True))

    nb = [a for a in f.kind("neighbor") if _d(a).get("near") == ip]
    if nb:
        rel = sum(1 for a in nb if _d(a).get("related"))
        group = Node([("", "соседи")], "group", anchor(ip, "nb"),
                     meta=[(_d(nb[0]).get("net") or "", "dim"), (f"{len(nb)} с именами", "dim")]
                     + ([(f"{rel} похож на ваш", "warn")] if rel else []))
        shown = sorted(nb, key=lambda a: (not _d(a).get("related"), _ip_key(a.value)))
        for a in shown[:8]:
            group.children.append(Node([(a.value, a.value)], "nb", meta=[(_d(a).get("ptr") or "", "")],
                                       tags=[("похож на ваш", "warn")] if _d(a).get("related") else []))
        if len(nb) > 8:
            group.children.append(Node([("", f"… ещё {len(nb) - 8}")], "more", dim=True, href="#found"))
        node.children.append(group)
    return node


def _port_node(f: Facts, ip: str, port: int, asset: Asset | None, urls: list[Url]) -> Node:
    d = _d(asset)
    u = f.best(urls)
    service = d.get("service") or SERVICES.get(port) or (u.scheme if u else "?")
    value = f"[{ip}]:{port}" if ":" in ip else f"{ip}:{port}"
    node = Node([(value, str(port))], "port", anchor(ip, port), cols=[(service, "err" if d.get("risky") else "")])
    if u is not None:
        w = u.data
        node.meta.append((str(w.get("status")), _status_tone(w.get("status"))))
        if u.scheme == "http" and w.get("status") in REDIRECTS:
            node.meta.append(("→ https" if w.get("to_https") else f"→ {w.get('location') or '?'}",
                              "ok" if w.get("to_https") else "dim"))
        if w.get("title"):
            node.meta.append((f"«{w['title']}»", ""))
        if w.get("server"):
            node.meta.append((w["server"], "dim"))
        if u.scheme == "https":
            cert = _d(f.certs.get(u.host))
            if cert.get("valid") is False:
                node.tags.append((f"tls: {cert.get('error') or 'не проходит проверку'}", "err"))
            elif cert.get("days_left") is not None:
                node.tags.append((f"tls {cert['days_left']} дн", _days_tone(cert["days_left"]) or "ok"))
            if cert.get("protocol"):
                node.meta.append((cert["protocol"], "warn" if cert["protocol"] in ("TLSv1", "TLSv1.1") else "dim"))
        elif not w.get("to_https"):
            node.tags.append(("без https", "warn"))
    elif d.get("banner"):
        node.meta.append((d["banner"], "dim"))
    if d.get("state") == "closed":
        node.dim = True
        node.tags.append(("закрыт", "dim"))
    elif d.get("risky"):
        node.tags.append(("наружу", "err"))
    # программы, которые работают на этом порту, и их уязвимости
    here = [t for t in f.techs.values() if (ip, port) in f.tech_spots(t)]
    total = 0
    for t in sorted(here, key=lambda t: (-len(f.vulns_of.get(t.value) or []), t.value))[:12]:
        vs = f.vulns_of.get(t.value) or []
        total += len(vs)
        c = Counter(_d(a).get("severity") for a in vs)
        tone = "err" if c["critical"] or c["high"] or any(_d(a).get("kev") for a in vs) else "warn" if vs else ""
        child = Node([("", t.value)], "tech", meta=[(_d(t).get("category", ""), "dim")],
                     href="#" + anchor("vulns", _d(t).get("product"), _d(t).get("version")) if vs else "")
        if vs:
            child.tags.append((f"CVE {len(vs)}", tone))
        if _d(t).get("eol_state") == "ended":  # ветка без поддержки: дыры в ней больше не чинят
            child.tags.insert(0, ("EOL", "err"))
        node.children.append(child)
    if len(here) > 12:
        node.children.append(Node([("", f"… ещё {len(here) - 12}")], "more", dim=True, href="#found"))
    if total:
        node.tags.append((f"CVE {total}", "err" if any(t[1] == "err" for c in node.children for t in c.tags) else "warn"))
    return node
