"""Первые модули разведки.

  Хост и провайдер   пассивный  IP, сеть и провайдер, CDN, кто держит DNS и почту
  Пинг и веб         активный   отвечает ли, как быстро, что за сервер, сертификат
  Открытые порты     активный   какие сервисы торчат наружу и что они о себе говорят
  Соседи по подсети  пассивный  кто живёт рядом в той же сети (по обратным DNS-именам)
"""

from __future__ import annotations

import asyncio
import ipaddress
import time
from contextlib import suppress
from statistics import mean

from ...core import fields as f
from ...core.modules import Kind, Module, ModuleError, ModuleSpec
from ...core.scope import ScopeError, normalize_host
from ...web.ui import plural
from . import netkit, ports as portlib, web


class HostInfo(Module):
    spec = ModuleSpec(
        id="hostinfo", title="Хост и провайдер", kind=Kind.PASSIVE, category="Разведка", order=10, icon="server",
        consumes=("domain",), produces=("ip", "dns_record"), timeout=120,
        description="IP-адреса сайта, сеть и провайдер (AS), страна, обратное имя, есть ли CDN. "
                    "Кто держит DNS и почту. Только DNS-запросы — сайт не трогаем",
        params=(f.boolean("www", "Проверить и www", default=True),),
    )

    async def run(self, run) -> None:
        res = netkit.resolver_for(run)
        run.log(f"[*] DNS: {res.note}", "dim")
        t = run.target
        ipmap = await netkit.resolve_names(run, res, netkit.target_names(run, www=run.params.get("www", True)))
        if t.is_ip:
            ipmap.setdefault(t.root, {"domains": [], "cnames": []})
        for rule in t.scope.include:
            if rule.network is not None and rule.network.num_addresses == 1:
                ipmap.setdefault(str(rule.network.network_address), {"domains": [], "cnames": []})
        if not ipmap:
            run.log("[!] ни одно имя цели не разрешилось в адрес — сайт не существует или DNS недоступен", "warn")
        await netkit.describe_ips(run, res, ipmap)

        dns_provider = mail = None
        if not t.is_ip:
            dns_provider, mail = await self._zone(run, res, t.root)

        ips = [ip for ip in ipmap]
        datas = {a["value"]: a["data"] for a in run.assets("ip")}
        own = [ip for ip in ips if not datas.get(ip, {}).get("cdn")]
        cdn = sorted({datas[ip]["cdn"] for ip in ips if datas.get(ip, {}).get("cdn")})
        hosting = next((netkit.org_name(datas[ip].get("as_name")) for ip in own if datas.get(ip, {}).get("as_name")), None)
        parts = [f"{len(ips)} {plural(len(ips), 'адрес', 'адреса', 'адресов')}"]
        if hosting:
            parts.append(f"хостинг {hosting}")
        if cdn:
            parts.append(f"за {', '.join(cdn)}")
        if dns_provider:
            parts.append(f"DNS {dns_provider}")
        if mail:
            parts.append(f"почта {mail}")
        run.summary = ", ".join(parts)
        run.log(f"[=] {run.summary}", "ok")

    async def _zone(self, run, res, root: str) -> tuple[str | None, str | None]:
        """NS и MX: у поддомена их обычно нет, поэтому поднимаемся до зоны."""
        zone, ns = root, []
        while True:
            ns, _err = await res.query(zone, "NS")
            if ns or zone.count(".") < 2:
                break
            zone = zone.split(".", 1)[1]
        mx, _err = await res.query(zone, "MX")
        run.log(f"[*] зона {zone}")
        dns_provider = mail = None
        if ns:
            dns_provider = next((p for p in (netkit.match(n, netkit.DNS_PROVIDERS) for n in ns) if p), None)
            run.log(f"    NS     {', '.join(sorted(ns))}" + (f"  → {dns_provider}" if dns_provider else ""))
            for n in ns:
                await run.emit("dns_record", f"NS {n}", type="NS", host=n, provider=netkit.match(n, netkit.DNS_PROVIDERS))
        else:
            run.log("    NS     не нашлось", "dim")
        if mx:
            hosts = []
            for row in sorted(mx, key=lambda r: int(r.split()[0]) if r.split()[0].isdigit() else 99):
                pref, _, host = row.partition(" ")
                hosts.append(host)
                await run.emit("dns_record", f"MX {pref} {host}", type="MX", host=host, priority=pref,
                               provider=netkit.match(host, netkit.MAIL_PROVIDERS))
            mail = next((p for p in (netkit.match(h, netkit.MAIL_PROVIDERS) for h in hosts) if p), None)
            run.log(f"    MX     {', '.join(sorted(mx))}" + (f"  → {mail}" if mail else ""))
        else:
            run.log("    MX     нет — почту на этом домене не принимают", "dim")
        return dns_provider, mail


class Reach(Module):
    spec = ModuleSpec(
        id="reach", title="Пинг и веб", kind=Kind.ACTIVE, category="Разведка", order=20, icon="activity",
        consumes=("domain", "ip"), produces=("ip", "url", "certificate", "subdomain", "domain"), timeout=180,
        description="Отвечает ли сайт и как быстро (TCP-пинг на 443 или 80), что за сервер, куда редиректит, "
                    "сертификат и сколько ему жить. Имена из сертификата — в находки",
        params=(
            f.integer("attempts", "Попыток пинга", default=4, min=1, max=20, width="half"),
            f.integer("timeout", "Ждать ответа, с", default=4, min=1, max=20, width="half"),
        ),
    )
    HTTPS_PORT = 443
    HTTP_PORT = 80

    async def run(self, run) -> None:
        res = netkit.resolver_for(run)
        timeout = float(run.params.get("timeout", 4))
        attempts = int(run.params.get("attempts", 4))
        ips = await netkit.ensure_ips(run, res)
        if not ips:
            run.log("[!] у цели нет адресов — сначала «Хост и провайдер»", "warn")
            return
        alive = 0
        rtts = []
        for ip in ips:
            port, samples, lost = None, [], 0
            for candidate in (self.HTTPS_PORT, self.HTTP_PORT):
                if not run.allows(ip, candidate)[0]:
                    continue
                try:
                    samples.append(await run.tcp_ping(ip, candidate, timeout))
                    port = candidate
                    break
                except (OSError, TimeoutError):
                    continue
            if port is None:
                run.log(f"[-] {ip:<15}  не отвечает ни на {self.HTTPS_PORT}, ни на {self.HTTP_PORT}", "warn")
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
            rtts.append(avg)
            run.log(f"[+] {ip:<15}  tcp/{port}  {min(samples):.1f} / {avg:.1f} / {max(samples):.1f} мс  "
                    f"потерь {lost}/{attempts}", "found")
            await run.emit("ip", ip, reachable=True, rtt_ms=round(avg, 1), rtt_port=port, loss=f"{lost}/{attempts}")

        notes = []
        if not run.target.is_ip:
            for name in netkit.target_names(run):
                hosts = {ip: d for ip, d in ips.items() if name in d.get("domains", [])}
                if hosts:
                    notes += await self._web(run, res, name, next(iter(hosts)), timeout)
        notes = list(dict.fromkeys(notes))  # одно и то же замечание с www и без — один раз
        avg = f", пинг {netkit.fmt_ms(min(rtts))}" if rtts else ""
        run.summary = f"отвечает {alive} из {len(ips)}{avg}" + (f"; {'; '.join(notes)}" if notes else "")

    async def _web(self, run, res, name: str, ip: str, timeout: float) -> list[str]:
        notes: list[str] = []
        run.log(f"[*] веб {name}")
        plain = await web.fetch(run, ip, name, self.HTTP_PORT, False, timeout=timeout)
        if "error" in plain:
            run.log(f"    http   {plain['error']}", "dim")
        else:
            hop = web.next_hop(plain["url"], plain.get("location", ""))
            if plain["status"] in (301, 302, 307, 308) and hop and hop[3]:
                run.log(f"    http   {plain['status']} → https", "ok")
            else:
                run.log(f"    [!] http:// не уводит на https:// (ответ {plain['status']})", "warn")
                notes.append("нет редиректа на https")

        port = self.HTTPS_PORT
        url, host, tls, path = f"https://{name}{'' if port == 443 else f':{port}'}/", name, True, "/"
        chain = []
        for _hop in range(6):
            r = await web.fetch(run, ip, host, port, tls, path, timeout=timeout)
            if "error" in r:
                run.log(f"    {url}: {r['error']}", "warn")
                if tls and _hop == 0 and "error" not in plain:  # https не работает, но http есть
                    notes.append("https не отвечает")
                break
            chain.append(f"{r['status']} {url}")
            if tls and r.get("tls"):
                await self._cert(run, host, r["tls"], notes)
            nxt = web.next_hop(url, r.get("location", "")) if r["status"] in (301, 302, 303, 307, 308) else None
            if nxt is None:
                await run.emit("url", url, status=r["status"], server=r["server"], powered_by=r["powered_by"],
                               title=r.get("title", ""), ms=r.get("ms"), hsts=r["hsts"], chain=chain,
                               tls_valid=(r.get("tls") or {}).get("valid"))
                bits = [f"{r['status']}", f"{r.get('ms', 0):.0f} мс"]
                if r["server"]:
                    bits.append(r["server"])
                run.log(f"    {url}  {'  '.join(bits)}", "found")
                if r.get("title"):
                    run.log(f"    title  {r['title']}")
                if r["powered_by"]:
                    run.log(f"    [i] X-Powered-By: {r['powered_by']} — сервер рассказывает, на чём работает")
                if tls and not r["hsts"]:
                    run.log("    [i] нет HSTS: браузер может однажды пойти на http://", "dim")
                break
            url, host, port, tls, path = nxt
            ok, reason = run.allows(host)
            if not ok:
                run.log(f"    → {url}: это уже не цель ({reason}), дальше не идём", "dim")
                break
            known = {a["value"]: a["data"] for a in run.assets("ip")}
            target_ip = next((i for i, d in known.items() if host in d.get("domains", [])), None)
            if target_ip is None:
                ipmap = await netkit.resolve_names(run, res, [host], quiet=True)
                await netkit.describe_ips(run, res, ipmap, quiet=True)
                target_ip = next(iter(ipmap), None)
            if target_ip is None:
                run.log(f"    → {url}: имя не разрешается", "warn")
                break
            run.log(f"    → {r['status']} {url}", "dim")
            ip = target_ip
        return notes

    async def _cert(self, run, host: str, info: dict, notes: list[str]) -> None:
        if not info.get("valid"):
            run.log(f"    [!] сертификат не прошёл проверку: {info.get('error')}", "err")
            notes.append("плохой сертификат")
            await run.emit("certificate", f"{host}:{self.HTTPS_PORT}", valid=False, error=info.get("error"))
            return
        days = info.get("days_left")
        tone = "err" if days is not None and days < 7 else "warn" if days is not None and days < 21 else "ok"
        run.log(f"    tls    {info.get('issuer')}, до {info.get('expires')} (осталось {days} дн)", tone)
        if tone != "ok":
            notes.append(f"сертификат кончается через {days} дн")
        await run.emit("certificate", f"{host}:{self.HTTPS_PORT}", valid=True, issuer=info.get("issuer"),
                       expires=info.get("expires"), days_left=days, sans=info.get("sans", []))
        root = run.target.root
        new_names = 0
        for san in info.get("sans", []):
            name = san.lstrip("*.").lower()
            try:
                name = normalize_host(name)
            except ScopeError:
                continue
            if name == root:
                continue
            kind = "subdomain" if name.endswith("." + root) else "domain"
            new_names += int(await run.emit(kind, name, source="сертификат", seen_on=host))
        if new_names:
            run.log(f"    [+] новых имён из сертификата: {new_names}", "found")


class OpenPorts(Module):
    spec = ModuleSpec(
        id="ports", title="Открытые порты", kind=Kind.ACTIVE, category="Сеть", order=30, icon="plug",
        consumes=("ip",), produces=("port",), timeout=900,
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
        res = netkit.resolver_for(run)
        candidates = list(await netkit.ensure_ips(run, res))
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

        host_of = {ip: (d.get("domains") or [""])[0] for ip, d in (await netkit.ensure_ips(run, res)).items()}
        risky = 0
        if opened and run.params.get("banners", True):
            run.log("[*] что отвечает на открытых портах")
        for ip, port, rtt in sorted(opened, key=lambda x: (x[0], x[1])):
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


class Neighbors(Module):
    spec = ModuleSpec(
        id="neighbors", title="Соседи по подсети", kind=Kind.PASSIVE, category="Сеть", order=40, icon="network",
        consumes=("ip",), produces=("neighbor",), timeout=300,
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
        res = netkit.resolver_for(run)
        ips = await netkit.ensure_ips(run, res)
        size = int(run.params.get("size", "24"))
        sem = asyncio.Semaphore(int(run.params.get("concurrency", 64)))
        root = run.target.root
        own = {d for data in ips.values() for d in data.get("domains", [])} | {root}
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
                if with_prefix.prefixlen > net.prefixlen and ipaddress.ip_address(ip) in with_prefix:
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
