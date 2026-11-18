"""Модули разведки. Один модуль — один вопрос.

  Адреса             пассивный  в какие IP смотрят имена цели (A, AAAA, CNAME)
  Провайдер          пассивный  чья сеть у адресов: AS, компания, страна; не CDN ли это
  Обратные имена     пассивный  как адреса называют себя (PTR)
  DNS-зона           пассивный  кто держит DNS, SOA, подписана ли зона, кто может выпускать сертификаты
  Почта              пассивный  куда идёт почта и защищён ли домен от поддельных писем (SPF, DMARC)
  Соседи по подсети  пассивный  кто ещё живёт в той же сети
  Пинг               активный   отвечают ли адреса и как быстро
  Ответ сайта        активный   код, сервер, страница, редиректы, защитные заголовки, cookie
  Сертификат         активный   проходит ли проверку, кем выдан, сколько жить, версия TLS
  Открытые порты     активный   какие сервисы торчат наружу и что о себе говорят
  Технологии         активный   какие программы и каких версий работают у цели
  Уязвимости (CVE)   пассивный  известные уязвимости этих версий: NVD, CISA KEV, EPSS

Что кому нужно, сказано в ModuleSpec.needs — движок сам запустит недостающее:
«Открытые порты» на новой цели сначала прогонят «Адреса» и «Провайдер».
"""

from __future__ import annotations

import asyncio
import ipaddress
import re
import secrets
import time
from collections import Counter
from contextlib import suppress
from statistics import mean

from ...core import fields as f
from ...core.modules import Kind, Module, ModuleError, ModuleSpec
from ...core.scope import ScopeError, normalize_host
from ...web.ui import plural
from . import fingerprints as fp, netkit, ports as portlib, versions, vulndb, web

REDIRECTS = (301, 302, 303, 307, 308)


def _www_param() -> f.Field:
    return f.boolean("www", "Проверить и www", default=True)


# --- DNS ---------------------------------------------------------------------------

class Resolve(Module):
    spec = ModuleSpec(
        id="resolve", title="Адреса", kind=Kind.PASSIVE, category="DNS", order=10, icon="globe",
        consumes=("domain",), produces=("ip",), timeout=60,
        description="В какие IP-адреса смотрят имена цели: записи A и AAAA, цепочка CNAME. На этих адресах "
                    "построены остальные модули. Если имя больше не указывает на старый адрес, модули его "
                    "больше не трогают — там может быть уже чужой сервер",
        params=(_www_param(),),
    )

    async def run(self, run) -> None:
        res = netkit.resolver_for(run)
        run.log(f"[*] DNS: {res.note}", "dim")
        t = run.target
        names = netkit.target_names(run, www=run.params.get("www", True))
        ipmap, sure = await netkit.resolve_names(run, res, names)
        fixed = [t.root] if t.is_ip else []  # адреса, вписанные в цель вручную
        fixed += [str(r.network.network_address) for r in t.scope.include
                  if r.network is not None and r.network.num_addresses == 1]
        for ip in fixed:
            ipmap.setdefault(ip, {"domains": [], "cnames": []})
            run.log(f"[*] {ip}: из хостов цели")
        for ip, entry in ipmap.items():
            await run.emit("ip", ip, domains=entry["domains"], cnames=entry["cnames"], private=netkit.private(ip),
                           version=ipaddress.ip_address(ip).version, gone=False)
        gone = []
        for a in run.assets("ip"):
            was = a["data"].get("domains") or []
            if a["value"] not in ipmap and was and all(name in sure for name in was):
                await run.emit("ip", a["value"], domains=[], gone=True, was=was)
                gone.append(a["value"])
        if gone:
            run.log(f"[!] имена цели больше не указывают на {', '.join(gone)} — эти адреса модули больше не трогают", "warn")
        if not ipmap:
            run.log("[!] ни одно имя не разрешилось в адрес: домена нет или DNS недоступен", "warn")
        n = len(ipmap)
        run.summary = (f"{n} {plural(n, 'адрес', 'адреса', 'адресов')}: " + ", ".join(list(ipmap)[:3])
                       + (" …" if n > 3 else "")) if n else "имена не разрешаются"
        if gone:
            run.summary += f"; больше не указывают: {len(gone)}"


class Provider(Module):
    spec = ModuleSpec(
        id="asn", title="Провайдер", kind=Kind.PASSIVE, category="Сеть", order=12, icon="server",
        consumes=("ip",), produces=("ip",), needs=("resolve",), timeout=120,
        description="Чья сеть у каждого адреса: номер AS, компания, префикс, страна (через DNS Team Cymru). "
                    "Узнаёт CDN — у адресов CDN порты не сканируются: это не сервер владельца сайта",
    )

    async def run(self, run) -> None:
        ips = netkit.own_ips(run)
        if not ips:
            return netkit.no_ips(run)
        res = netkit.resolver_for(run)

        async def one(ip: str):
            return ip, (None if netkit.private(ip) else await res.asn(ip))

        cdns, hosting, unknown = set(), [], 0
        for ip, info in await asyncio.gather(*(one(ip) for ip in ips)):
            if netkit.private(ip):
                await run.emit("ip", ip, checked=True, private=True, cdn=None)
                run.log(f"[*] {ip:<15}  частная сеть — провайдера нет", "dim")
                continue
            if info is None:
                await run.emit("ip", ip, checked=False)
                run.log(f"[-] {ip:<15}  сеть не определилась: Team Cymru не ответил", "warn")
                unknown += 1
                continue
            cdn = netkit.cdn_of(info.get("as_name"), ips[ip].get("cnames") or [])
            await run.emit("ip", ip, checked=True, cdn=cdn, asn=info.get("asn"), as_name=info.get("as_name"),
                           prefix=info.get("prefix"), country=info.get("country"), registry=info.get("registry"))
            org = netkit.org_name(info.get("as_name")) or "?"
            run.log(f"[+] {ip:<15}  AS{info.get('asn')}  {org}  {info.get('prefix')}  {info.get('country')}", "found")
            if cdn:
                cdns.add(cdn)
                run.log(f"    [!] {cdn} — чужая инфраструктура: сайт за ней, порты этого адреса не трогаем", "warn")
            elif org not in hosting:
                hosting.append(org)
        parts = []
        if hosting:
            parts.append("хостинг " + ", ".join(hosting))
        if cdns:
            parts.append("за " + ", ".join(sorted(cdns)))
        if unknown:
            parts.append(f"сеть не определилась у {unknown} {plural(unknown, 'адреса', 'адресов', 'адресов')}")
        run.summary = ", ".join(parts) or "частная сеть"


class ReverseNames(Module):
    spec = ModuleSpec(
        id="ptr", title="Обратные имена", kind=Kind.PASSIVE, category="DNS", order=14, icon="tag",
        consumes=("ip",), produces=("ip",), needs=("resolve",), timeout=60,
        description="Как адреса цели называют себя в обратной зоне DNS (PTR). Часто это имя сервера "
                    "у хостинга — по нему видно, чей это сервер и какой он",
    )

    async def run(self, run) -> None:
        ips = netkit.own_ips(run)
        if not ips:
            return netkit.no_ips(run)
        res = netkit.resolver_for(run)
        names = await asyncio.gather(*(res.ptr(ip) for ip in ips))
        for ip, name in zip(ips, names):
            await run.emit("ip", ip, ptr=name)
            run.log(f"[{'+' if name else '-'}] {ip:<15}  {name or 'без обратного имени'}", "found" if name else "dim")
        named = sum(1 for n in names if n)
        run.summary = f"имена у {named} из {len(ips)}"


class Zone(Module):
    spec = ModuleSpec(
        id="zone", title="DNS-зона", kind=Kind.PASSIVE, category="DNS", order=16, icon="list",
        consumes=("domain",), produces=("dns_record",), timeout=60,
        description="Кто держит DNS домена (NS), главный сервер зоны (SOA), подписана ли зона (DNSSEC) "
                    "и кому разрешено выпускать сертификаты на домен (CAA)",
    )

    async def run(self, run) -> None:
        if run.target.is_ip:
            run.log("[-] цель — IP-адрес: DNS-зоны у неё нет", "dim")
            run.summary = "цель — IP-адрес"
            return
        res = netkit.resolver_for(run)
        zone, ns = await netkit.find_zone(res, run.target.root)
        if not ns:
            run.log(f"[!] у {run.target.root} не нашлось NS: домен не делегирован или DNS недоступен", "warn")
            run.summary = "NS не найдены"
            return
        provider = next((p for p in (netkit.match(n, netkit.DNS_PROVIDERS) for n in ns) if p), None)
        run.log(f"[*] зона {zone}")
        run.log(f"    NS      {'  '.join(ns)}" + (f"  → {provider}" if provider else ""), "found")
        for n in ns:
            await run.emit("dns_record", f"NS {n}", type="NS", zone=zone, host=n,
                           provider=netkit.match(n, netkit.DNS_PROVIDERS), detail=n)

        soa, _err = await res.query(zone, "SOA")
        if soa:
            mname, rname, serial = (soa[0].split() + ["", "", ""])[:3]
            email = rname.replace(".", "@", 1) if rname and "@" not in rname else rname
            run.log(f"    SOA     {mname}  {email}  serial {serial}")
            await run.emit("dns_record", f"SOA {zone}", type="SOA", zone=zone, host=mname, email=email, serial=serial,
                           detail=f"{mname}  {email}  serial {serial}")

        keys, _err = await res.query(zone, "DNSKEY")
        signed = bool(keys)
        run.log(f"    DNSSEC  {'зона подписана' if signed else 'не подписана'}", "ok" if signed else "dim")
        await run.emit("dns_record", f"DNSSEC {zone}", type="DNSSEC", zone=zone, signed=signed,
                       detail="зона подписана" if signed else "не подписана")

        caa: list[str] = []
        for name in dict.fromkeys([run.target.root, zone]):  # CAA ищут от имени вверх по дереву
            caa, _err = await res.query(name, "CAA")
            if caa:
                break
        issuers = sorted({(v.split(" ", 2)[2].split(";")[0].strip().strip('"') or "никому")
                          for v in caa if len(v.split(" ", 2)) == 3 and v.split(" ", 2)[1] in ("issue", "issuewild")})
        if caa:
            run.log(f"    CAA     выпускать сертификаты можно: {', '.join(issuers) or '—'}", "ok")
        else:
            run.log("    CAA     нет — сертификат на домен может выпустить любой удостоверяющий центр", "dim")
        await run.emit("dns_record", f"CAA {zone}", type="CAA", zone=zone, issuers=issuers, records=caa,
                       detail=", ".join(issuers) if caa else "нет")
        run.summary = (f"DNS {provider or 'свои серверы'}, DNSSEC {'есть' if signed else 'нет'}, "
                       f"CAA {'есть' if caa else 'нет'}")


def spf_all(record: str) -> str:
    """Чем кончается SPF: «-all» (чужим нельзя), «~all», «?all», «+all» (можно всем) или redirect=…"""
    for token in record.split()[1:]:
        low = token.lower()
        if re.fullmatch(r"[+\-~?]?all", low):
            return low if low[0] in "+-~?" else "+all"
        if low.startswith("redirect="):
            return low
    return ""


def dmarc_tags(record: str) -> dict[str, str]:
    tags = {}
    for part in record.split(";"):
        if "=" in part:
            k, v = part.split("=", 1)
            tags[k.strip().lower()] = v.strip()
    return tags


class Mail(Module):
    spec = ModuleSpec(
        id="mail", title="Почта", kind=Kind.PASSIVE, category="DNS", order=18, icon="mail",
        consumes=("domain",), produces=("dns_record",), timeout=60,
        description="Куда приходит почта домена (MX и чей это сервис) и защищён ли домен от поддельных писем "
                    "от его имени: SPF (кто вправе отправлять) и DMARC (что делать с подделкой)",
    )

    async def run(self, run) -> None:
        if run.target.is_ip:
            run.log("[-] цель — IP-адрес: почты домена у неё нет", "dim")
            run.summary = "цель — IP-адрес"
            return
        res = netkit.resolver_for(run)
        zone, _ns = await netkit.find_zone(res, run.target.root)
        run.log(f"[*] почта домена {zone}")

        mx, _err = await res.query(zone, "MX")
        mail = None
        rows = sorted(mx, key=lambda r: int(r.split()[0]) if r.split()[0].isdigit() else 99)
        for row in rows:
            pref, _, host = row.partition(" ")
            provider = netkit.match(host, netkit.MAIL_PROVIDERS)
            mail = mail or provider
            await run.emit("dns_record", f"MX {pref} {host}", type="MX", zone=zone, host=host, priority=pref,
                           provider=provider, detail=f"{pref} {host}")
        if rows:
            run.log(f"    MX      {'  '.join(rows)}" + (f"  → {mail}" if mail else ""), "found")
        else:
            run.log("    MX      нет — почту на этом домене не принимают", "dim")

        txt, _err = await res.query(zone, "TXT")
        spf = [t for t in txt if t.lower().startswith("v=spf1")]
        policy = spf_all(spf[0]) if spf else ""
        await run.emit("dns_record", f"SPF {zone}", type="SPF", zone=zone, records=spf, all=policy,
                       detail=spf[0] if len(spf) == 1 else (f"записей {len(spf)}" if spf else "нет"))
        if len(spf) > 1:
            run.log(f"    [!] SPF     записей {len(spf)} — по стандарту должна быть одна, сейчас не работает ни одна", "err")
        elif spf:
            run.log(f"    SPF     {spf[0]}", "ok" if policy in ("-all", "~all") or policy.startswith("redirect=") else "warn")
        else:
            run.log("    [!] SPF     нет — отправлять письма от имени домена может кто угодно", "warn")

        dtxt, _err = await res.query(f"_dmarc.{zone}", "TXT")
        dmarc = [t for t in dtxt if t.lower().startswith("v=dmarc1")]
        tags = dmarc_tags(dmarc[0]) if dmarc else {}
        await run.emit("dns_record", f"DMARC {zone}", type="DMARC", zone=zone, records=dmarc,
                       policy=tags.get("p", "").lower(), rua=bool(tags.get("rua")), detail=dmarc[0] if dmarc else "нет")
        if dmarc:
            run.log(f"    DMARC   {dmarc[0]}", "ok" if tags.get("p", "").lower() in ("reject", "quarantine") else "warn")
        else:
            run.log("    [!] DMARC   нет — с поддельными письмами от имени домена никто ничего не делает", "warn")

        run.summary = ", ".join([f"почта {mail or ('свой сервер' if rows else 'не принимается')}",
                                 f"SPF {policy or ('есть' if spf else 'нет')}" if len(spf) < 2 else "SPF сломан",
                                 f"DMARC p={tags.get('p', '?')}" if dmarc else "DMARC нет"])


class Neighbors(Module):
    spec = ModuleSpec(
        id="neighbors", title="Соседи по подсети", kind=Kind.PASSIVE, category="Сеть", order=20, icon="network",
        consumes=("ip",), produces=("neighbor",), needs=("resolve", "asn"), timeout=300,
        description="Кто ещё живёт в той же сети: обратные DNS-имена соседних адресов. Соседей с похожими "
                    "именами (mail., dev., другой сайт владельца) предложит добавить в цель. "
                    "Спрашиваем DNS, к соседям не подключаемся",
        params=(
            f.choice("size", "Сколько соседей смотреть", {"24": "/24 — 256 адресов", "26": "/26 — 64 адреса",
                                                         "28": "/28 — 16 адресов"}, default="24", width="half"),
            f.integer("concurrency", "Одновременно запросов", default=64, min=1, max=256, width="half"),
        ),
    )

    async def run(self, run) -> None:
        ips = netkit.own_ips(run)
        if not ips:
            return netkit.no_ips(run)
        res = netkit.resolver_for(run)
        size = int(run.params.get("size", "24"))
        sem = asyncio.Semaphore(int(run.params.get("concurrency", 64)))
        own = {d for data in ips.values() for d in data.get("domains") or []} | {run.target.root}
        total_named = related = 0

        async def ptr(addr: str):
            async with sem:
                return addr, await res.ptr(addr)

        for ip, data in ips.items():
            addr = ipaddress.ip_address(ip)
            if addr.version != 4:
                run.log(f"[-] {ip}: соседей в IPv6 перебором не найти", "dim")
                continue
            if data.get("cdn"):
                run.log(f"[-] {ip}: это {data['cdn']} — соседи там чужие сайты, пропускаю", "dim")
                continue
            if addr.is_loopback:
                continue
            net = ipaddress.ip_network(f"{ip}/{size}", strict=False)
            if data.get("prefix"):
                with_prefix = ipaddress.ip_network(data["prefix"], strict=False)
                if with_prefix.prefixlen > net.prefixlen and addr in with_prefix:
                    net = with_prefix  # сеть провайдера уже /24 — не выходим за неё
            hosts = [str(h) for h in net.hosts()] or [ip]
            run.log(f"[*] {ip}: смотрю {net} — {len(hosts)} адресов")
            named = []
            done = 0
            for fut in asyncio.as_completed([ptr(h) for h in hosts if h != ip]):
                addr_s, name = await fut
                done += 1
                run.progress(done, len(hosts) - 1, f"соседей {net}")
                if name:
                    named.append((addr_s, name))
            named.sort(key=lambda x: ipaddress.ip_address(x[0]))
            for addr_s, name in named:
                mine = any(name == d or name.endswith("." + d) for d in own)
                related += int(mine)
                await run.emit("neighbor", addr_s, ptr=name, near=ip, net=str(net), related=mine)
                if mine:
                    run.log(f"[!] {addr_s:<15}  {name}  — похоже, тоже ваш", "warn")
            for addr_s, name in named[:30]:
                if not any(name == d or name.endswith("." + d) for d in own):
                    run.log(f"    {addr_s:<15}  {name}")
            if len(named) > 30:
                run.log(f"    … и ещё {len(named) - 30} — все в «Найдено»", "dim")
            total_named += len(named)
        run.summary = f"соседей с именами {total_named}" + (f", похожих на ваши {related}" if related else "")


# --- активные: обращаются к адресам цели ---------------------------------------------------

class Ping(Module):
    spec = ModuleSpec(
        id="ping", title="Пинг", kind=Kind.ACTIVE, category="Сеть", order=30, icon="activity",
        consumes=("ip",), produces=("ip",), needs=("resolve",), timeout=120,
        description="Отвечают ли адреса цели и как быстро: TCP-подключение к 443, а если он закрыт — к 80. "
                    "ICMP часто режут, а TCP-порт сайта открыт всегда. Показывает потери и разброс",
        params=(
            f.integer("attempts", "Попыток", default=4, min=1, max=20, width="half"),
            f.integer("timeout", "Ждать ответа, с", default=4, min=1, max=20, width="half"),
        ),
    )
    PORTS = (443, 80)

    async def run(self, run) -> None:
        ips = netkit.own_ips(run)
        if not ips:
            return netkit.no_ips(run)
        timeout = float(run.params.get("timeout", 4))
        attempts = int(run.params.get("attempts", 4))
        alive, best = 0, []
        for ip in ips:
            port, samples, lost = None, [], 0
            for candidate in self.PORTS:
                if not run.allows(ip, candidate)[0]:
                    continue
                try:
                    samples.append(await run.tcp_ping(ip, candidate, timeout))
                    port = candidate
                    break
                except (OSError, TimeoutError):
                    continue
            if port is None:
                run.log(f"[-] {ip:<15}  не отвечает ни на {' ни на '.join(map(str, self.PORTS))}", "warn")
                await run.emit("ip", ip, reachable=False)
                continue
            for _ in range(attempts - 1):
                await asyncio.sleep(0.2)
                try:
                    samples.append(await run.tcp_ping(ip, port, timeout))
                except (OSError, TimeoutError):
                    lost += 1
            alive += 1
            avg = mean(samples)
            best.append(min(samples))
            run.log(f"[+] {ip:<15}  tcp/{port}  {min(samples):.1f} / {avg:.1f} / {max(samples):.1f} мс  "
                    f"потерь {lost}/{attempts}", "found")
            await run.emit("ip", ip, reachable=True, rtt_ms=round(avg, 1), rtt_port=port, loss=f"{lost}/{attempts}")
        run.summary = f"отвечает {alive} из {len(ips)}" + (f", пинг {netkit.fmt_ms(min(best))}" if best else "")


class Site(Module):
    spec = ModuleSpec(
        id="http", title="Ответ сайта", kind=Kind.ACTIVE, category="Веб", order=32, icon="file",
        consumes=("domain", "ip"), produces=("url", "ip"), needs=("resolve",), timeout=180,
        description="Что отвечает сайт по http и https: код, сервер, заголовок страницы, куда ведут редиректы, "
                    "уводит ли http на https, какие защитные заголовки есть и с какими флагами ставятся cookie. "
                    "Редиректы за пределы цели не открываются",
        params=(_www_param(), f.integer("timeout", "Ждать ответа, с", default=6, min=1, max=30)),
    )
    HTTP_PORT = 80
    HTTPS_PORT = 443

    async def run(self, run) -> None:
        pairs = netkit.names_with_ip(run, www=run.params.get("www", True))
        if not pairs:
            return netkit.no_ips(run)
        timeout = float(run.params.get("timeout", 6))
        res = netkit.resolver_for(run)
        notes: list[str] = []
        first = ""
        for name, ip in pairs:
            status, more = await self._name(run, res, name, ip, timeout)
            first = first or status
            notes += more
        notes = list(dict.fromkeys(notes))  # одно и то же у example.com и www — один раз
        run.summary = "; ".join([first or "не отвечает", *notes])

    async def _name(self, run, res, name: str, ip: str, timeout: float) -> tuple[str, list[str]]:
        run.log(f"[*] {name}  ({ip})")
        notes: list[str] = []
        plain = await web.fetch(run, ip, name, self.HTTP_PORT, False, timeout=timeout)
        if "error" in plain:
            run.log(f"    http   {plain['error']}", "dim")
        else:
            hop = web.next_hop(plain["url"], plain.get("location", ""))
            to_https = bool(plain["status"] in REDIRECTS and hop and hop[3])
            await run.emit("url", plain["url"], status=plain["status"], server=plain["server"],
                           location=plain.get("location", ""), to_https=to_https, title=plain.get("title", ""),
                           ms=plain.get("ms"))
            if to_https:
                run.log(f"    http   {plain['status']} → https", "ok")
            else:
                run.log(f"    [!] http:// не уводит на https:// (ответ {plain['status']})", "warn")
                notes.append("http не уводит на https")

        port = self.HTTPS_PORT
        url, host, tls, path = f"https://{name}{'' if port == 443 else f':{port}'}/", name, True, "/"
        chain: list[str] = []
        status = ""
        for hop_n in range(6):
            r = await web.fetch(run, ip, host, port, tls, path, timeout=timeout)
            if "error" in r:
                run.log(f"    {url}: {r['error']}", "warn")
                if tls and hop_n == 0 and "error" not in plain:
                    notes.append("https не отвечает")
                break
            chain.append(f"{r['status']} {url}")
            nxt = web.next_hop(url, r.get("location", "")) if r["status"] in REDIRECTS else None
            if nxt is None:
                await run.emit("url", url, status=r["status"], server=r["server"], powered_by=r["powered_by"],
                               title=r.get("title", ""), ms=r.get("ms"), hsts=r["hsts"], chain=chain,
                               tls_valid=(r.get("tls") or {}).get("valid"), missing=r.get("missing", []),
                               cookies=r.get("cookies", []))
                status = f"{'https' if tls else 'http'} {r['status']}" + (f" «{r['title']}»" if r.get("title") else "")
                bits = [str(r["status"]), f"{r.get('ms', 0):.0f} мс"] + ([r["server"]] if r["server"] else [])
                run.log(f"    {url}  {'  '.join(bits)}", "found")
                if r.get("title"):
                    run.log(f"    title  {r['title']}")
                if r["powered_by"]:
                    run.log(f"    [i] X-Powered-By: {r['powered_by']} — сервер рассказывает, на чём работает")
                if tls and not r["hsts"]:
                    run.log("    [i] нет HSTS: браузер может однажды пойти на http://", "dim")
                if r.get("missing"):
                    run.log(f"    [i] нет защитных заголовков: {', '.join(r['missing'])}", "dim")
                insecure = [c["name"] for c in r.get("cookies", []) if tls and not c["secure"]]
                if insecure:
                    run.log(f"    [!] cookie без флага Secure: {', '.join(insecure)}", "warn")
                break
            url, host, port, tls, path = nxt
            ok, reason = run.allows(host)
            if not ok:
                run.log(f"    → {url}: это уже не цель ({reason}), дальше не идём", "dim")
                break
            known = {ip_: d for ip_, d in netkit.own_ips(run).items() if host in (d.get("domains") or [])}
            target_ip = next(iter(known), None)
            if target_ip is None:  # редирект на другое имя цели: узнаём его адрес
                ipmap, _sure = await netkit.resolve_names(run, res, [host], quiet=True)
                for new_ip, entry in ipmap.items():
                    await run.emit("ip", new_ip, domains=entry["domains"], cnames=entry["cnames"],
                                   private=netkit.private(new_ip), version=ipaddress.ip_address(new_ip).version)
                target_ip = next(iter(ipmap), None)
            if target_ip is None:
                run.log(f"    → {url}: имя не разрешается", "warn")
                break
            run.log(f"    → {r['status']} {url}", "dim")
            ip = target_ip
        return status, notes


class Certificate(Module):
    spec = ModuleSpec(
        id="tls", title="Сертификат", kind=Kind.ACTIVE, category="Веб", order=34, icon="lock",
        consumes=("domain", "ip"), produces=("certificate", "subdomain", "domain"), needs=("resolve",), timeout=120,
        description="TLS-сертификат каждого имени цели: проходит ли проверку (и если нет — почему, по-человечески), "
                    "кем выдан, сколько ему осталось, версия TLS и шифр. Другие имена в сертификате — подсказки: "
                    "поддомены и соседние сайты владельца",
        params=(_www_param(), f.integer("timeout", "Ждать ответа, с", default=6, min=1, max=30)),
    )
    PORT = 443

    async def run(self, run) -> None:
        pairs = netkit.names_with_ip(run, www=run.params.get("www", True))
        if not pairs:
            return netkit.no_ips(run)
        timeout = float(run.params.get("timeout", 6))
        notes: list[str] = []
        new_names = 0
        for name, ip in pairs:
            info = await web.tls_probe(run, ip, name, self.PORT, timeout)
            value = f"{name}:{self.PORT}"
            if "failed" in info:
                run.log(f"[-] {name}: TLS не отвечает — {info['failed']}", "warn")
                await run.emit("certificate", value, valid=None, failed=info["failed"], error=None)
                notes.append(f"{name}: нет https")
                continue
            tech = {"protocol": info.get("protocol"), "cipher": info.get("cipher"), "bits": info.get("bits")}
            if not info["valid"]:
                run.log(f"[!] {name}: {info['error']}  ({info.get('raw')})", "err")
                await run.emit("certificate", value, valid=False, error=info["error"], raw=info.get("raw"),
                               failed=None, **tech)
                notes.append(f"{name}: {info['error']}")
            else:
                days = info.get("days_left")
                tone = "err" if days is not None and days < 7 else "warn" if days is not None and days < 21 else "ok"
                run.log(f"[+] {name}: {info.get('issuer')}, до {info.get('expires')} (осталось {days} дн)", tone)
                await run.emit("certificate", value, valid=True, issuer=info.get("issuer"), subject=info.get("subject"),
                               expires=info.get("expires"), days_left=days, sans=info.get("sans", []),
                               error=None, raw=None, failed=None, **tech)
                if tone != "ok":
                    notes.append(f"кончается через {days} дн")
                new_names += await self._names(run, name, info.get("sans", []))
            run.log(f"    {tech['protocol'] or '?'}  {tech['cipher'] or '?'}" + (f"  {tech['bits']} бит" if tech["bits"] else ""),
                    "warn" if tech["protocol"] in ("TLSv1", "TLSv1.1") else "dim")
            if tech["protocol"] in ("TLSv1", "TLSv1.1"):
                notes.append(f"устаревший {tech['protocol']}")
        if new_names:
            run.log(f"[+] новых имён из сертификатов: {new_names}", "found")
        notes = list(dict.fromkeys(notes))
        valid = sum(1 for a in run.assets("certificate") if a["data"].get("valid"))
        run.summary = "; ".join([f"проходит проверку {valid} из {len(pairs)}", *notes])

    async def _names(self, run, host: str, sans: list[str]) -> int:
        root = run.target.root
        new = 0
        for san in sans:
            name = san.lstrip("*.").lower()
            try:
                name = normalize_host(name)
            except ScopeError:
                continue
            if name == root:
                continue
            kind = "subdomain" if name.endswith("." + root) else "domain"
            new += int(await run.emit(kind, name, source="сертификат", seen_on=host))
        return new


class OpenPorts(Module):
    spec = ModuleSpec(
        id="ports", title="Открытые порты", kind=Kind.ACTIVE, category="Сеть", order=40, icon="plug",
        consumes=("ip",), produces=("port",), needs=("resolve", "asn"), timeout=900,
        description="TCP-подключения к портам адресов цели: что открыто и что сервис о себе говорит. "
                    "Адреса CDN и чужих платформ пропускаются — это не сервер владельца",
        params=(
            f.choice("preset", "Какие порты", portlib.PRESETS, default="top", width="half"),
            f.text("custom", "Свои порты", placeholder="22, 80, 443, 8000-8100", width="half",
                   help="Для варианта «Свои»"),
            f.integer("concurrency", "Одновременно подключений", default=200, min=1, max=1000, width="half"),
            f.integer("timeout_ms", "Ждать ответа, мс", default=800, min=100, max=5000, width="half"),
            f.boolean("banners", "Узнавать, что отвечает на порту", default=True),
            f.boolean("subnets", "Сканировать подсети из хостов цели (до /24)", default=False),
        ),
    )

    async def run(self, run) -> None:
        try:
            port_list = portlib.ports_for(run.params.get("preset", "top"), run.params.get("custom", ""))
        except ValueError as exc:
            raise ModuleError(f"список портов: {exc}") from None
        if not port_list:
            run.log("[!] список портов пуст", "warn")
            return
        own = netkit.own_ips(run)
        candidates = list(own)
        if run.params.get("subnets"):
            for rule in run.target.scope.include:
                net = rule.network
                if net is not None and net.num_addresses > 1:
                    if net.num_addresses > 256:
                        run.log(f"[-] {net}: больше /24 — пропускаю, разбейте на части", "warn")
                        continue
                    candidates += [str(h) for h in net.hosts()]
        ips = []
        for ip in dict.fromkeys(candidates):
            ok, reason = run.allows(ip)
            if ok:
                ips.append(ip)
            else:
                run.log(f"[-] {ip}: {reason}", "dim")
        if not ips:
            run.log("[!] нет адресов, которые можно сканировать", "warn")
            run.summary = "нечего сканировать: адреса цели за CDN или не найдены"
            return

        timeout = int(run.params.get("timeout_ms", 800)) / 1000
        sem = asyncio.Semaphore(int(run.params.get("concurrency", 200)))
        total = len(ips) * len(port_list)
        run.log(f"[*] {len(ips)} {plural(len(ips), 'адрес', 'адреса', 'адресов')} × {len(port_list)} портов, "
                f"таймаут {timeout * 1000:.0f} мс")

        async def probe(ip: str, port: int):
            async with sem:
                start = time.perf_counter()
                try:
                    _r, writer = await run.connect(ip, port, timeout)
                except (OSError, TimeoutError):
                    return None
                rtt = (time.perf_counter() - start) * 1000
                writer.close()
                with suppress(Exception):
                    await asyncio.wait_for(writer.wait_closed(), 0.5)
                return ip, port, rtt

        done = 0
        opened: list[tuple[str, int, float]] = []
        for fut in asyncio.as_completed([probe(ip, p) for ip in ips for p in port_list]):
            hit = await fut
            done += 1
            run.progress(done, total, "портов")
            if hit:
                opened.append(hit)
                run.log(f"[+] {hit[0]}:{hit[1]}  открыт", "found")
        run.progress(total, total, "портов")

        host_of = {ip: (d.get("domains") or [""])[0] for ip, d in own.items()}
        risky = 0
        if opened and run.params.get("banners", True):
            run.log("[*] что отвечает на открытых портах")
        for ip, port, rtt in sorted(opened, key=lambda x: (ipaddress.ip_address(x[0]), x[1])):
            text, tls = ("", False)
            if run.params.get("banners", True):
                text, tls = await portlib.banner(run, ip, port, host_of.get(ip, ""), max(timeout, 1.5))
            service = portlib.SERVICES.get(port, "")
            danger = port in portlib.RISKY
            risky += int(danger)
            value = f"[{ip}]:{port}" if ":" in ip else f"{ip}:{port}"
            await run.emit("port", value, ip=ip, port=port, service=service, banner=text, tls=tls, rtt_ms=round(rtt, 1),
                           risky=danger, state="open")
            line = f"    {value:<22} {service or '?':<14} {text}".rstrip()
            run.log(line + ("   ← не должно смотреть наружу" if danger else ""), "err" if danger else "info")

        # порты, которые раньше были открыты, а теперь нет
        open_now = {(ip, p) for ip, p, _ in opened}
        closed = 0
        for a in run.assets("port"):
            d = a["data"]
            if d.get("ip") in ips and d.get("port") in port_list and (d["ip"], d["port"]) not in open_now \
                    and d.get("state") != "closed":
                await run.emit("port", a["value"], state="closed")
                closed += 1
        if closed:
            run.log(f"[i] закрылось с прошлого раза: {closed}", "warn")
        run.summary = (f"открыто {len(opened)} {plural(len(opened), 'порт', 'порта', 'портов')} "
                       f"на {len(ips)} {plural(len(ips), 'адресе', 'адресах', 'адресах')}"
                       + (f", опасных {risky}" if risky else ""))


# --- что работает у цели и что в этом известно плохого ------------------------------------

# сервисы, которые молчат, пока их не спросят: безопасные вопросы «кто ты и какой версии»
SERVICE_PROBES = {6379: b"INFO server\r\n", 11211: b"version\r\n"}
HTTP_PROBES = {9200: "/", 2375: "/version", 2376: "/version"}


class Technologies(Module):
    spec = ModuleSpec(
        id="tech", title="Технологии", kind=Kind.ACTIVE, category="Уязвимости", order=44, icon="layers",
        consumes=("domain", "ip", "port"), produces=("technology",), needs=("resolve", "ports"), timeout=900,
        description="Какие программы и каких версий работают у цели: веб-сервер, язык, CMS с плагинами и темами, "
                    "панели, базы, почта, SSH, FTP, JS-библиотеки на страницах. Читает приветствия сервисов, "
                    "заголовки, страницу и страницу ошибки, спрашивает пару десятков служебных адресов, где программы "
                    "сами называют версию. Это основа для поиска уязвимостей",
        params=(
            f.boolean("paths", "Спрашивать служебные адреса с версией", default=True),
            f.boolean("wordpress", "Плагины и темы WordPress", default=True),
            f.integer("timeout", "Ждать ответа, с", default=6, min=1, max=30, width="half"),
            f.integer("parallel", "Одновременно запросов", default=4, min=1, max=16, width="half"),
        ),
    )

    async def run(self, run) -> None:
        own = netkit.own_ips(run)
        if not own:
            return netkit.no_ips(run)
        timeout = float(run.params.get("timeout", 6))
        self.sem = asyncio.Semaphore(int(run.params.get("parallel", 4)))
        found: list[tuple[fp.Hit, str]] = []
        open_ports = [a["data"] for a in run.assets("port")
                      if a["data"].get("ip") in own and a["data"].get("state") != "closed" and a["data"].get("port")]

        # 1) что сервисы сказали сами, когда «Открытые порты» к ним подключались
        for p in open_ports:
            for h in fp.from_banner(p.get("banner") or ""):
                found.append((h, self._addr(p["ip"], p["port"])))
        # 2) сервисы, которые молчат, пока не спросишь
        for p in open_ports:
            where = self._addr(p["ip"], p["port"])
            if p["port"] in SERVICE_PROBES:
                text = await self._ask(run, p["ip"], p["port"], SERVICE_PROBES[p["port"]], timeout)
                found += [(h, where) for h in fp.from_banner(text)]
            if p["port"] in HTTP_PROBES:
                r = await web.fetch(run, p["ip"], p["ip"], p["port"], bool(p.get("tls")), HTTP_PROBES[p["port"]],
                                    timeout=timeout, keep=True)
                found += [(h, where) for h in fp.from_path(HTTP_PROBES[p["port"]], r.get("body", ""))]
        # 3) веб: главная, страница ошибки, служебные адреса, скрипты, WordPress
        done: set[tuple[str, int]] = set()
        for host, ip, port, tls in self._origins(run, own, open_ports):
            found += await self._web(run, host, ip, port, tls, timeout, done)

        techs = self._merge(found)
        seen = set()
        for t in techs:
            prod = fp.product(t["product"])
            if prod is None:
                continue
            value = f"{prod.title} {t['version']}".strip()
            seen.add(value)
            precision = "exact" if t["version"] and versions.precise(t["version"]) else "partial" if t["version"] else "none"
            await run.emit("technology", value, product=prod.id, title=prod.title, version=t["version"],
                           category=prod.category, where=t["where"], evidence=t["evidence"], distro=t["distro"],
                           package=t["package"], precision=precision, cve_lookup=prod.checkable, state="seen")
            where = ", ".join(t["where"][:2]) + (f" +{len(t['where']) - 2}" if len(t["where"]) > 2 else "")
            extra = f"  [{t['distro']}{' ' + t['package'] if t['package'] else ''}]" if t["distro"] else ""
            run.log(f"[+] {value:<34} {where}{extra}", "found" if t["version"] else "info")
            if t["evidence"]:
                run.log(f"      {t['evidence'][0]}", "dim")
        gone = 0
        for a in run.assets("technology"):
            if a["value"] not in seen and a["data"].get("state") != "gone":
                await run.emit("technology", a["value"], state="gone")
                gone += 1
        if gone:
            run.log(f"[i] больше не видно: {gone} — обновили или убрали", "warn")
        with_version = [t for t in techs if t["version"]]
        run.summary = (f"программ {len(techs)}, с версией {len(with_version)}"
                       + (": " + ", ".join(f"{fp.product(t['product']).title} {t['version']}" for t in with_version[:5])
                          if with_version else ""))

    # --- откуда смотреть -------------------------------------------------------------
    @staticmethod
    def _addr(ip: str, port: int) -> str:
        return f"[{ip}]:{port}" if ":" in ip else f"{ip}:{port}"

    @staticmethod
    def _origins(run, own: dict, open_ports: list[dict]) -> list[tuple[str, str, int, bool]]:
        """(имя, адрес, порт, TLS) — где искать веб. Порты: открытые веб-порты адреса, а если их не сканировали
        (адрес CDN) — 80 и 443."""
        by_ip: dict[str, list[tuple[int, bool]]] = {}
        for p in open_ports:
            port = int(p["port"])
            if port in (80, 443) or port in portlib.HTTP_PORTS or port in portlib.TLS_PORTS or p.get("tls"):
                if port not in HTTP_PROBES:  # Elasticsearch и Docker спрашиваем отдельно
                    by_ip.setdefault(p["ip"], []).append((port, bool(p.get("tls")) or port in portlib.TLS_PORTS or port == 443))
        out = []
        for name, ip in netkit.names_with_ip(run):
            for port, tls in sorted(by_ip.get(ip) or [(443, True), (80, False)], key=lambda x: (not x[1], x[0])):
                out.append((name, ip, port, tls))
        return out

    # --- веб -------------------------------------------------------------------------
    async def _get(self, run, host: str, ip: str, port: int, tls: bool, path: str, timeout: float,
                   headers: dict[str, str] | None = None) -> dict:
        async with self.sem:
            return await web.fetch(run, ip, host, port, tls, path, timeout=timeout, keep=True, extra_headers=headers)

    async def _web(self, run, host: str, ip: str, port: int, tls: bool, timeout: float, done: set) -> list:
        url = f"{'https' if tls else 'http'}://{host}{'' if port == (443 if tls else 80) else f':{port}'}"
        r = await self._get(run, host, ip, port, tls, "/", timeout)
        for _hop in range(3):  # редирект на тот же сайт (http → https, / → /ru/) — идём следом
            nxt = web.next_hop(r.get("url", url), r.get("location", "")) if r.get("status") in REDIRECTS else None
            if nxt is None or nxt[1] != host:
                break
            _u, _h, port, tls, path = nxt
            url = f"{'https' if tls else 'http'}://{host}{'' if port == (443 if tls else 80) else f':{port}'}"
            r = await self._get(run, host, ip, port, tls, path, timeout)
        if (host, port) in done or "error" in r:
            if "error" in r:
                run.log(f"[-] {url}: {r['error']}", "dim")
            return []
        done.add((host, port))
        run.log(f"[*] {url}")
        found = [(h, url) for h in fp.from_headers(r.get("headers", {}), r.get("cookies", []))]
        body = r.get("body", "")
        found += [(h, url) for h in fp.from_body(body)]

        # страница ошибки часто называет сервер и версию, даже когда заголовки молчат
        miss = await self._get(run, host, ip, port, tls, f"/perimeter-{secrets.token_hex(4)}", timeout)
        found += [(h, url) for h in fp.from_headers(miss.get("headers", {}), [])]
        found += [(h, url) for h in fp.from_body(miss.get("body", ""))]

        if run.params.get("paths", True):
            async def probe(path: str) -> list:
                a = await self._get(run, host, ip, port, tls, path, timeout)
                if "error" in a or a.get("status", 0) >= 500 or a.get("status") in (404, 410):
                    return []
                hits = fp.from_path(path, a.get("body", ""))
                if a.get("status") == 200 and "html" in (a.get("headers") or {}).get("content-type", ""):
                    hits += fp.from_body(a.get("body", ""))
                hits += fp.from_headers(a.get("headers", {}), a.get("cookies", []))
                return [(h, f"{url}{path}") for h in hits]

            for chunk in await asyncio.gather(*(probe(p) for p in fp.PROBE_PATHS)):
                found += chunk

        found += await self._script_banners(run, host, ip, port, tls, body, found, url, timeout)
        if run.params.get("wordpress", True) and any(h.product == "wordpress" for h, _ in found):
            found += await self._wordpress(run, host, ip, port, tls, body, url, timeout)
        return found

    async def _script_banners(self, run, host, ip, port, tls, body: str, found: list, url: str, timeout: float) -> list:
        """Библиотека подключена без версии в адресе (/js/jquery.min.js) — читаем шапку файла."""
        versioned = {h.product for h, _ in found if h.version}
        out = []
        for src in fp.urls_of(body)[:200]:
            path = src
            if src.startswith(("http://", "https://", "//")):
                parts = web.next_hop(url + "/", src)
                if parts is None or parts[1] != host:
                    continue  # чужой CDN — его версию уже видно из адреса, если она там есть
                path = parts[4]
            if not path.startswith("/"):
                path = "/" + path
            for pid, name_rx, banner_rx in fp.FILE_BANNERS:
                if pid in versioned or not name_rx.search(path):
                    continue
                r = await self._get(run, host, ip, port, tls, path, timeout, {"Range": "bytes=0-2047"})
                m = banner_rx.search(r.get("body", ""))
                if m:
                    out.append((fp.Hit(pid, m.group("v"), f"шапка {path}: {m.group(0)[:60]}"), url))
                    versioned.add(pid)
        return out

    async def _wordpress(self, run, host, ip, port, tls, body: str, url: str, timeout: float) -> list:
        """Плагины и темы WordPress: из адресов на странице, версия — из readme.txt и style.css."""
        plugins, themes = fp.wp_assets(body)
        out = []
        # ?ver= без версии плагина WordPress подставляет свою: такой номер про ядро, не про плагин
        core = next((h.version for h in fp.from_body(body) if h.product == "wordpress" and h.version), "")

        async def plugin(slug: str, ver: str):
            r = await self._get(run, host, ip, port, tls, f"/wp-content/plugins/{slug}/readme.txt", timeout,
                                {"Range": "bytes=0-4095"})
            m = fp.STABLE_TAG.search(r.get("body", "")) if r.get("status") in (200, 206) else None
            if m:
                version, how = m.group("v"), f"readme.txt: Stable tag {m.group('v')}"
            elif ver and ver == core:
                version, how = "", f"?ver={ver} — это версия самого WordPress, своей плагин не назвал"
            else:
                version, how = ver, (f"?ver={ver} в адресе" if ver else "на странице")
            return fp.Hit(f"wp-plugin:{slug}", version, f"плагин {slug}, {how}")

        async def theme(slug: str, ver: str):
            r = await self._get(run, host, ip, port, tls, f"/wp-content/themes/{slug}/style.css", timeout,
                                {"Range": "bytes=0-2047"})
            m = fp.THEME_VERSION.search(r.get("body", "")) if r.get("status") in (200, 206) else None
            version = m.group("v") if m else "" if ver == core else ver
            return fp.Hit(f"wp-theme:{slug}", version, f"тема {slug}, " + ("style.css" if m else "адрес на странице"))

        tasks = [plugin(s, v) for s, v in list(plugins.items())[:40]] + [theme(s, v) for s, v in list(themes.items())[:5]]
        for h in await asyncio.gather(*tasks):
            out.append((h, url))
        if plugins or themes:
            run.log(f"    WordPress: плагинов {len(plugins)}, тем {len(themes)}", "dim")
        return out

    @staticmethod
    async def _ask(run, ip: str, port: int, payload: bytes, timeout: float) -> str:
        try:
            reader, writer = await run.connect(ip, port, timeout)
        except (OSError, TimeoutError):
            return ""
        try:
            writer.write(payload)
            await writer.drain()
            data = await asyncio.wait_for(reader.read(4096), timeout)
            return data.decode("utf-8", "replace")
        except (OSError, TimeoutError, asyncio.IncompleteReadError):
            return ""
        finally:
            writer.close()
            with suppress(Exception):
                await asyncio.wait_for(writer.wait_closed(), 1)

    # --- свести находки --------------------------------------------------------------
    @staticmethod
    def _merge(found: list[tuple[fp.Hit, str]]) -> list[dict]:
        """Одна программа одной версии — одна запись со всеми местами и признаками. Версия из файла, который
        мог остаться от старой установки (readme.html), уступает свежим признакам."""
        best: dict[tuple[str, str], int] = {}
        for h, where in found:
            if h.version:
                key = (h.product, where.split("/", 3)[2] if "://" in where else where)
                best[key] = max(best.get(key, 0), h.weight)
        versioned = {h.product for h, _ in found if h.version}
        merged: dict[tuple[str, str], dict] = {}
        for h, where in found:
            site = where.split("/", 3)[2] if "://" in where else where
            if not h.version and h.product in versioned:
                continue  # «PHP» без версии, когда есть «PHP 8.1.2», — тот же PHP
            if h.version and h.weight < best.get((h.product, site), 0):
                continue
            t = merged.setdefault((h.product, h.version), {"product": h.product, "version": h.version, "where": [],
                                                             "evidence": [], "distro": "", "package": ""})
            place = where.rstrip("/") if "://" in where else where
            if "://" in place:
                place = "/".join(place.split("/")[:3])
            if place not in t["where"]:
                t["where"].append(place)
            if h.evidence and h.evidence not in t["evidence"] and len(t["evidence"]) < 5:
                t["evidence"].append(h.evidence)
            t["distro"] = t["distro"] or h.distro
            t["package"] = t["package"] or h.package
        order = {"веб-сервер": 0, "сервер приложений": 1, "прокси": 2, "язык": 3, "CMS": 4}
        return sorted(merged.values(), key=lambda t: (order.get(getattr(fp.product(t["product"]), "category", ""), 9),
                                                      t["product"], t["version"]))


class Vulnerabilities(Module):
    spec = ModuleSpec(
        id="cve", title="Уязвимости (CVE)", kind=Kind.PASSIVE, category="Уязвимости", order=46, icon="shield-alert",
        consumes=("technology",), produces=("vulnerability", "technology"), needs=("tech",), timeout=3600,
        description="Известные уязвимости найденных версий: по каждой программе — вся база NVD, а не выборка "
                    "(у плагинов и тем WordPress — их собственная база), с оценкой опасности (CVSS), признаком "
                    "«уже используют в атаках» (CISA KEV), вероятностью атаки (EPSS) и версией, где исправлено. "
                    "Сайт не трогает — спрашивает открытые базы",
        params=(
            f.choice("min", "Показывать", {"0": "все уязвимости", "4": "от средней опасности",
                                           "7": "только высокие и критические"}, default="0"),
            f.boolean("all_versions", "Учитывать записи NVD без версий («все версии»)", default=False,
                      help="Обычно это старые описания вроде «пакеты RHEL 5 за 2008 год» — много ложных совпадений"),
        ),
    )

    async def run(self, run) -> None:
        techs = [a for a in run.assets("technology") if a["data"].get("state") != "gone"]
        if not techs:
            run.log("[!] программ с версиями не найдено — нечего сверять", "warn")
            run.summary = "программ не найдено"
            return
        db = vulndb.VulnDB(run.data_dir / "vulndb", run.setting, say=run.log)
        min_score = float(run.params.get("min", "0") or 0)
        all_versions = bool(run.params.get("all_versions"))
        vague = 0
        results = []
        async with db.client() as client:
            kev = await db.kev(client) if run.setting("vulndb.use_kev") else {}
            if kev:
                run.log(f"[*] каталог CISA KEV (уязвимости, которые уже используют в атаках): записей {len(kev)}", "dim")
            if not run.setting("vulndb.nvd_api_key"):
                run.log("[i] NVD без ключа отвечает раз в 6 секунд — первая проверка небыстрая, дальше из кеша", "dim")
            for i, t in enumerate(techs):
                run.progress(i, len(techs), "программ")
                d = t["data"]
                prod = fp.product(d.get("product", ""))
                version = d.get("version") or ""
                if prod is None or not prod.checkable:
                    continue
                if not version:
                    run.log(f"[-] {prod.title}: версия не видна — по версии уязвимости не определить", "dim")
                    continue
                if not versions.precise(version):
                    run.log(f"[-] {prod.title} {version}: версия неточная — сверять опасно, будут ложные совпадения", "dim")
                    continue
                items: dict[str, dict] = {}
                closed = ""
                if prod.wp:  # плагин или тема WordPress: у них своя база, NVD их почти не знает по названию
                    try:
                        got = await db.wordpress(client, prod.wp, prod.id.partition(":")[2])
                        items = {c["id"]: c for c in got["items"]}
                        closed = got["closed"]
                    except vulndb.VulnDBError as exc:
                        run.log(f"    [!] {prod.title}: {exc}", "err")
                for vp in prod.cpes:
                    try:
                        for c in await db.product(client, vp, prod.target_sw):
                            if c["id"] in items:
                                items[c["id"]]["matches"] = items[c["id"]]["matches"] + c["matches"]
                            else:
                                items[c["id"]] = c
                    except vulndb.VulnDBError as exc:
                        run.log(f"    [!] {prod.title}: {exc}", "err")
                matched = []
                for c in items.values():
                    how = [h for ok, h in (versions.affected(m, version) for m in c["matches"]) if ok]
                    if not how or (c["cvss"] or 0) < min_score:
                        continue
                    how = "exact" if "exact" in how else "range" if "range" in how else "all"
                    if how == "all" and not all_versions:
                        vague += 1
                        continue
                    matched.append((c, how))
                results.append((t, prod, version, _dedupe(matched, version), len(items), closed))
                run.log(f"[*] {prod.title} {version}: {'база WordPress' if prod.wp else 'NVD'} — записей {len(items)}, "
                        f"к этой версии — {len(results[-1][3])}", "dim")
            run.progress(len(techs), len(techs), "программ")
            ids = list(dict.fromkeys(c["id"] for *_, matched, _n, _c in results for c, _ in matched
                                     if c["id"].startswith("CVE-")))
            epss = await db.epss(client, ids) if ids and run.setting("vulndb.use_epss") else {}

        seen: set[str] = set()
        total = Counter()
        exploited = 0
        for t, prod, version, matched, known, closed in results:
            d = t["data"]
            matched.sort(key=lambda x: (not (kev.get(x[0]["id"]) or x[0]["kev"]), -(x[0]["cvss"] or 0), x[0]["id"]))
            counts = Counter(c["severity"] for c, _ in matched)
            needs, in_kev, no_fix = [], 0, 0  # needs: (версия, «новее неё»), чтобы закрыть всё известное
            for c, how in matched:
                k = kev.get(c["id"]) or c.get("kev") or {}
                e = epss.get(c["id"])
                fix = versions.fixed_in(c["matches"], version)
                if fix.startswith("после "):  # «уязвимы до 8.0.40 включительно» — нужна любая новее
                    needs.append((fix[len("после "):], True))
                elif fix:
                    needs.append((fix, False))
                else:  # верхней границы нет вовсе: исправления не называют
                    no_fix += 1
                in_kev += bool(k)
                await run.emit("vulnerability", c["id"], product=prod.id, product_title=prod.title, version=version,
                               tech=t["value"], where=d.get("where") or [], cvss=c["cvss"], severity=c["severity"],
                               vector=c["vector"], cvss_version=c["cvss_version"], summary=c["summary"],
                               published=c["published"], cwe=c["cwe"], exploit=c["exploit"], kev=bool(k),
                               kev_added=k.get("added", ""), kev_name=k.get("name", ""),
                               ransomware=bool(k.get("ransomware")), epss=e[0] if e else None,
                               epss_pct=e[1] if e else None, fixed_in=fix, how=how, distro=d.get("distro", ""),
                               unfixed=bool(c.get("unfixed")), title=c.get("title", ""), refs=c["refs"],
                               link=_vuln_link(c), state="open")
                seen.add(c["id"])
            order = versions.sort_key()
            upgrade, after = max(needs, key=lambda x: (order(x[0]), x[1])) if needs else ("", False)
            await run.emit("technology", t["value"], cves=len(matched), critical=counts["critical"], high=counts["high"],
                           medium=counts["medium"], low=counts["low"], unknown=counts["unknown"], kev=in_kev,
                           upgrade_to=upgrade, upgrade_after=after, no_fix=no_fix, closed=closed,
                           cve_source="WordPress" if prod.wp else "NVD", cve_checked=True)
            total.update(counts)
            exploited += in_kev
            self._report(run, prod, version, d, matched, counts, in_kev,
                         (f"новее {upgrade}" if after else upgrade) if upgrade else "", known, kev, epss)
            if closed:
                run.log(f"      [!] {prod.title} убран из каталога WordPress ({closed}) — обновлений больше не будет, "
                        "замените его", "err")

        if vague:
            run.log(f"[i] записей NVD без указания версий («все версии»): {vague} — не учтены, обычно это старые "
                    "описания; включить можно в параметрах", "dim")
        closed = 0
        for a in run.assets("vulnerability"):
            if a["value"] not in seen and a["data"].get("state") != "closed":
                await run.emit("vulnerability", a["value"], state="closed")
                closed += 1
        if closed:
            run.log(f"[i] закрыто с прошлого раза: {closed}", "ok")
        n = sum(total.values())
        run.summary = (f"уязвимостей {n}" + (f": критических {total['critical']}, высоких {total['high']}" if n else "")
                       + (f", в атаках {exploited}" if exploited else "") + f"; запросов к базам {db.requests}")

    @staticmethod
    def _report(run, prod, version, d, matched, counts, in_kev, upgrade, known, kev, epss) -> None:
        where = ", ".join((d.get("where") or [])[:2])
        if not matched:
            base = "в базе WordPress" if prod.wp else "в NVD"
            run.log(f"[ok] {prod.title} {version}: известных уязвимостей нет ({base} по продукту {known})", "ok")
            return
        parts = [f"{vulndb.SEVERITY_MANY[s]} {counts[s]}" for s in vulndb.SEVERITY_MANY if counts[s]]
        level = "err" if counts["critical"] or counts["high"] or in_kev else "warn"
        run.log(f"[!] {prod.title} {version}  {where}: уязвимостей {len(matched)} — {', '.join(parts)}"
                + (f"; в атаках {in_kev}" if in_kev else "") + (f"; обновить до {upgrade}" if upgrade else ""), level)
        if d.get("distro"):
            run.log(f"      сборка {d['distro']}{' ' + d['package'] if d.get('package') else ''}: часть исправлений "
                    "могли перенести без смены версии — сверяйте с бюллетенями дистрибутива", "dim")
        for c, _how in matched[:8]:
            k = kev.get(c["id"]) or c.get("kev")
            e = epss.get(c["id"])
            tags = (" [в атаках]" if k else "") + (" [эксплойт]" if c["exploit"] else "") \
                + (f" EPSS {e[0] * 100:.0f}%" if e and e[0] >= 0.01 else "")
            score = f"{c['cvss']:.1f}" if c["cvss"] is not None else " — "
            run.log(f"      {c['id']:<16} {score:>4} {vulndb.SEVERITY_RU[c['severity']]:<11}{tags}  {c['summary'][:90]}",
                    "err" if c["severity"] == "critical" or k else "warn" if c["severity"] == "high" else "info")
        if len(matched) > 8:
            run.log(f"      … и ещё {len(matched) - 8} — все в разделе «Уязвимости»", "dim")


_KIND_NOISE = re.compile(r"\([^)]*\)|\bvulnerabilit(?:y|ies)\b|[^a-z ]+")


def _kind(title: str) -> str:
    """Вид дыры из заголовка базы WordPress: «Contact Form 7 < 5.3.2 - Unrestricted File Upload» и
    «… plugin <= 5.3.1 is vulnerable to Unrestricted File Upload» → «unrestricted file upload»."""
    low = title.lower()
    if " is vulnerable to " in low:
        low = low.split(" is vulnerable to ", 1)[1]
    elif " - " in low:
        low = low.rsplit(" - ", 1)[1]
    else:
        return ""
    return " ".join(_KIND_NOISE.sub(" ", low.replace("-", " ")).split())


def _dedupe(matched: list[tuple[dict, str]], version: str) -> list[tuple[dict, str]]:
    """Базы WordPress описывают одну дыру несколькими записями (Wordfence, Patchstack, WPScan), с CVE и без.
    Запись без CVE того же вида и с тем же исправлением, что уже есть, — повтор: его не показываем."""
    def key(c: dict) -> tuple[str, str] | None:
        k = _kind(c.get("title", ""))
        return (k, versions.fixed_in(c["matches"], version)) if k else None

    seen = {key(c) for c, _ in matched if c["id"].startswith("CVE-")} - {None}
    out = []
    for c, how in sorted(matched, key=lambda x: not x[0]["id"].startswith("CVE-")):
        k = key(c)
        if not c["id"].startswith("CVE-") and k is not None:
            if k in seen:
                continue
            seen.add(k)
        out.append((c, how))
    return out


def _vuln_link(c: dict) -> str:
    if c["id"].startswith("CVE-"):
        return f"https://nvd.nist.gov/vuln/detail/{c['id']}"
    return next((r["url"] for r in c.get("refs") or [] if str(r.get("url", "")).startswith(("https://", "http://"))), "")


MODULES = (Resolve, Provider, ReverseNames, Zone, Mail, Neighbors, Ping, Site, Certificate, OpenPorts,
           Technologies, Vulnerabilities)
