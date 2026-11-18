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

from ...web.ui import plural
from ..runs.models import Asset, Run
from . import netkit
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

    def kind(self, k: str) -> list[Asset]:
        return self.by.get(k, [])

    def did(self, module_id: str) -> bool:
        return self.ran is None or module_id in self.ran

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
    return [g for g in (_net(f), _site(f), _dns(f), _outside(f)) if g.facts]


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
    return sorted(merged.values(), key=lambda i: (rank[i.level], i.where, i.title))


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
    return node
