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

Что кому нужно, сказано в ModuleSpec.needs — движок сам запустит недостающее:
«Открытые порты» на новой цели сначала прогонят «Адреса» и «Провайдер».
"""

from __future__ import annotations

import asyncio
import ipaddress
import re
import time
from contextlib import suppress
from statistics import mean

from ...core import fields as f
from ...core.modules import Kind, Module, ModuleError, ModuleSpec
from ...core.scope import ScopeError, normalize_host
from ...web.ui import plural
from . import netkit, ports as portlib, web

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


MODULES = (Resolve, Provider, ReverseNames, Zone, Mail, Neighbors, Ping, Site, Certificate, OpenPorts)
