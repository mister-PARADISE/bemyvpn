"""Как показать цель человеку: сводка, замечания, структура.

Всё строится из находок (Asset), в сеть ничего не ходит. Модули пишут факты, а здесь решается,
что из них главное, что плохо и как это связано: имя → адрес → порт → что на нём отвечает.
"""

from __future__ import annotations

import ipaddress
import re
from collections import Counter
from dataclasses import dataclass, field
from typing import Any
from urllib.parse import urlsplit

from sqlalchemy import select

from ...web.ui import plural
from ..runs.models import Asset
from . import netkit
from .ports import RISKY, SERVICES

Seg = tuple[str, str]  # (текст, оттенок: "" | ok | warn | err | dim)

REDIRECTS = (301, 302, 303, 307, 308)


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
    """Находки цели, разложенные по полочкам. Строится один раз на страницу."""

    def __init__(self, target, rows: list[Asset]) -> None:
        self.target = target
        self.root: str = target.domain  # основной домен (или IP) цели
        self.rows = rows
        self.by: dict[str, list[Asset]] = {}
        for a in rows:
            self.by.setdefault(a.kind, []).append(a)
        self.ips: dict[str, Asset] = {a.value: a for a in sorted(self.kind("ip"), key=lambda a: _ip_key(a.value))}
        self.names: dict[str, list[str]] = {}  # имя → его адреса
        for ip, a in self.ips.items():
            for name in _d(a).get("domains") or []:
                self.names.setdefault(name, []).append(ip)
        self.ports: dict[str, dict[int, Asset]] = {}
        for a in self.kind("port"):
            ip, port = _d(a).get("ip"), _d(a).get("port")
            if ip and port:
                self.ports.setdefault(ip, {})[int(port)] = a
        self.urls: list[Url] = []
        for a in self.kind("url"):
            u = urlsplit(a.value)
            if u.hostname:
                self.urls.append(Url(a, u.scheme, u.hostname, u.port or (443 if u.scheme == "https" else 80)))
        self.certs = {a.value.rsplit(":", 1)[0]: a for a in self.kind("certificate")}

    def kind(self, k: str) -> list[Asset]:
        return self.by.get(k, [])

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
        db.info[key] = Facts(target, rows)
    return db.info[key]


# --- сводка ------------------------------------------------------------------------

@dataclass
class Fact:
    key: str
    value: list[Seg]
    sub: str = ""
    href: str = ""


@dataclass
class Group:
    title: str
    facts: list[Fact]
    module: tuple[str, str] | None = None  # чем заполнить, если пусто: (id модуля, название)


def summary(f: Facts) -> list[Group]:
    return [_net(f), _site(f), _dns(f), _outside(f)]


def _net(f: Facts) -> Group:
    g = Group("сеть", [], ("hostinfo", "Хост и провайдер"))
    if not f.ips:
        return g
    addrs = list(f.ips)
    g.facts.append(Fact("адреса", [(ip, "") for ip in addrs[:3]] + ([(f"+{len(addrs) - 3}", "dim")] if len(addrs) > 3 else []),
                        href="#tree"))
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
    g.facts.append(Fact("cdn", [(", ".join(cdn), "warn")] if cdn else [("нет", "")],
                        sub="" if cdn else "сервер виден напрямую"))
    rtts = [_d(a)["rtt_ms"] for a in f.ips.values() if _d(a).get("rtt_ms") is not None]
    dead = [ip for ip, a in f.ips.items() if _d(a).get("reachable") is False]
    if rtts or dead:
        value = [(netkit.fmt_ms(min(rtts)), "")] if rtts else []
        if dead:
            value.append((f"не отвечает {len(dead)}" if rtts else "не отвечает", "err"))
        g.facts.append(Fact("пинг", value))
    return g


def _site(f: Facts) -> Group:
    g = Group("сайт", [], ("reach", "Пинг и веб"))
    https, http = f.site()
    page = https or (http if http and not http.data.get("to_https") else None)
    if page is None and http is None:
        return g
    if page is not None:
        d = page.data
        g.facts.append(Fact("ответ", [(str(d.get("status")), _status_tone(d.get("status"))), (d.get("server") or "", "dim"),
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
        g.facts.append(Fact("hsts", [("есть", "ok")] if https.data.get("hsts") else [("нет", "dim")]))
        cert = f.certs.get(https.host)
        if cert is not None:
            c = _d(cert)
            if c.get("valid") is False:
                g.facts.append(Fact("tls", [("не проходит проверку", "err")], sub=c.get("error") or ""))
            else:
                days = c.get("days_left")
                g.facts.append(Fact("tls", [(c.get("issuer") or "?", ""), (f"до {c.get('expires')}", "dim"),
                                            (f"{days} дн", _days_tone(days) or "ok")],
                                    sub=f"имён в сертификате {len(c.get('sans') or [])}" if c.get("sans") else ""))
    return g


def _dns(f: Facts) -> Group:
    g = Group("dns и почта", [], ("hostinfo", "Хост и провайдер"))
    recs = f.kind("dns_record")
    ns = sorted({_d(a).get("host") or a.value for a in recs if _d(a).get("type") == "NS"})
    mx = sorted((a for a in recs if _d(a).get("type") == "MX"), key=lambda a: int(_d(a).get("priority") or 99))
    if not ns and not mx:
        return g
    dns_by = Counter(_d(a).get("provider") for a in recs if _d(a).get("type") == "NS" and _d(a).get("provider"))
    g.facts.append(Fact("dns", [(dns_by.most_common(1)[0][0] if dns_by else ("свои серверы" if ns else "?"), "")],
                        sub="  ".join(ns)))
    if mx:
        mail = next((_d(a).get("provider") for a in mx if _d(a).get("provider")), None)
        g.facts.append(Fact("почта", [(mail or "свой сервер", "")], sub="  ".join(_d(a).get("host") or "" for a in mx)))
    else:
        g.facts.append(Fact("почта", [("не принимает", "dim")], sub="нет MX-записей"))
    return g


def _outside(f: Facts) -> Group:
    g = Group("снаружи", [], ("ports", "Открытые порты"))
    ports = [a for per in f.ports.values() for a in per.values()]
    if ports:
        opened = [a for a in ports if _d(a).get("state") != "closed"]
        nums: dict[int, bool] = {}
        for a in opened:
            nums[int(_d(a)["port"])] = nums.get(int(_d(a)["port"]), False) or bool(_d(a).get("risky"))
        hosts = len({_d(a)["ip"] for a in opened})
        g.facts.append(Fact("порты", [(str(p), "err" if bad else "") for p, bad in sorted(nums.items())] or [("все закрыты", "dim")],
                            sub=f"открыто {len(opened)} на {hosts} {plural(hosts, 'адресе', 'адресах', 'адресах')}" if opened else "",
                            href="#tree"))
        risky = sorted({(int(_d(a)["port"]), _d(a).get("service") or RISKY.get(int(_d(a)["port"]), "")) for a in opened
                        if _d(a).get("risky")})
        g.facts.append(Fact("опасные", [(f"{p} {s}".strip(), "err") for p, s in risky] or [("нет", "ok")],
                            href="#issues" if risky else ""))
        closed = sorted({int(_d(a)["port"]) for a in ports if _d(a).get("state") == "closed"})
        if closed:
            g.facts.append(Fact("закрылись", [(str(p), "dim") for p in closed], sub="были открыты в прошлый раз"))
    nb = f.kind("neighbor")
    if nb:
        rel = sum(1 for a in nb if _d(a).get("related"))
        g.facts.append(Fact("соседи", [(f"{len(nb)} с именами", "")] + ([(f"{rel} похож на ваш", "warn")] if rel else []),
                            href="#tree"))
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
    for host, c in f.certs.items():
        d = _d(c)
        if d.get("valid") is False:
            out.append(Issue("err", "сертификат не проходит проверку", host,
                             f"{d.get('error') or ''}. Браузер покажет посетителю «Подключение не защищено».".lstrip(". "),
                             f.node_of(host)))
        elif d.get("days_left") is not None and d["days_left"] < 21:
            out.append(Issue("err" if d["days_left"] < 7 else "warn", f"сертификат кончается через {d['days_left']} дн", host,
                             f"Действует до {d.get('expires')} ({d.get('issuer')}). Не продлят — сайт перестанет "
                             "открываться без предупреждения.", f.node_of(host)))
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
    if https is not None:
        d = https.data
        if not d.get("hsts"):
            out.append(Issue("info", "нет HSTS", https.host, "Браузер не запоминает, что сайт только на https, — "
                             "первый заход могут перехватить. Заголовок Strict-Transport-Security.", f.node_of(https.host, https.port)))
        if d.get("powered_by"):
            out.append(Issue("info", f"сервер называет движок: {d['powered_by']}", https.host,
                             "По версии ищут известные уязвимости. Заголовок X-Powered-By лучше убрать.",
                             f.node_of(https.host, https.port)))
        if re.search(r"/\d", d.get("server") or ""):
            out.append(Issue("info", f"видна версия сервера: {d['server']}", https.host,
                             "По версии ищут известные уязвимости. В nginx — server_tokens off.", f.node_of(https.host, https.port)))
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

    recs = f.kind("dns_record")
    if recs:
        node = Node([("", "dns")], "group", anchor("dns"))
        by_type: dict[str, list[Asset]] = {}
        for a in recs:
            by_type.setdefault((_d(a).get("type") or a.value.split()[0]).upper(), []).append(a)
        for typ in sorted(by_type, key=lambda x: ({"NS": 0, "MX": 1}.get(x, 2), x)):
            rows = by_type[typ]
            provider = Counter(_d(a).get("provider") for a in rows if _d(a).get("provider")).most_common(1)
            if typ == "MX":
                rows.sort(key=lambda a: int(_d(a).get("priority") or 99))
                values = [f"{_d(a).get('priority')} {_d(a).get('host')}" for a in rows]
            else:
                values = sorted(_d(a).get("host") or a.value.split(" ", 1)[-1] for a in rows)
            child = Node([("", typ.lower())], "rec", meta=[(provider[0][0], "")] if provider else [])
            child.meta += [(v, "dim") for v in values]
            node.children.append(child)
        root.children.append(node)
    return root


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
                node.tags.append(("tls не проходит проверку", "err"))
            elif cert.get("days_left") is not None:
                node.tags.append((f"tls {cert['days_left']} дн", _days_tone(cert["days_left"]) or "ok"))
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
