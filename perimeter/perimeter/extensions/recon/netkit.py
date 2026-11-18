"""Сетевой набор разведки: DNS, ASN через Team Cymru, кто провайдер, кто CDN.

Всё здесь пассивное: вопросы задаются DNS-серверам, а не сайту.
"""

from __future__ import annotations

import asyncio
import ipaddress
from dataclasses import dataclass, field
from typing import Any

import dns.asyncresolver
import dns.exception
import dns.nameserver
import dns.resolver
import dns.reversename

# --- кто есть кто: по имени сети (AS) и по CNAME ---------------------------------------
# «Чужая инфраструктура»: адрес принадлежит CDN или платформе, а не серверу владельца сайта.
# Порты таких адресов не сканируем — это была бы проверка чужой сети.

CDN_BY_AS_NAME = (
    ("cloudflare", "Cloudflare"), ("akamai", "Akamai"), ("fastly", "Fastly"), ("ddos-guard", "DDoS-Guard"),
    ("qrator", "Qrator"), ("stormwall", "StormWall"), ("incapsula", "Imperva"), ("imperva", "Imperva"),
    ("sucuri", "Sucuri"), ("cdn77", "CDN77"), ("datacamp", "CDN77"), ("ngenix", "Ngenix"), ("edgecast", "Edgio"),
    ("edgio", "Edgio"), ("bunny", "bunny.net"), ("g-core", "Gcore"), ("gcore", "Gcore"), ("cdnvideo", "CDNvideo"),
    ("tilda", "Tilda"),
)
CDN_BY_CNAME = (
    ("cloudflare.net", "Cloudflare"), ("akamaiedge.net", "Akamai"), ("akamaized.net", "Akamai"),
    ("edgekey.net", "Akamai"), ("edgesuite.net", "Akamai"), ("fastly.net", "Fastly"), ("fastlylb.net", "Fastly"),
    ("cloudfront.net", "Amazon CloudFront"), ("azureedge.net", "Azure CDN"), ("azurefd.net", "Azure Front Door"),
    ("b-cdn.net", "bunny.net"), ("ngenix.net", "Ngenix"), ("cdnvideo.ru", "CDNvideo"), ("gcdn.co", "Gcore"),
    ("incapdns.net", "Imperva"), ("sucuri.net", "Sucuri"), ("ddos-guard.net", "DDoS-Guard"),
    ("stormwall.pro", "StormWall"), ("cdn77.org", "CDN77"), ("vercel-dns.com", "Vercel"), ("netlify.app", "Netlify"),
    ("github.io", "GitHub Pages"), ("tilda.ws", "Tilda"),
)
DNS_PROVIDERS = (
    ("cloudflare.com", "Cloudflare"), ("yandex.net", "Яндекс"), ("yandex.ru", "Яндекс"), ("reg.ru", "REG.RU"),
    ("nic.ru", "RU-CENTER"), ("timeweb.ru", "Timeweb"), ("timeweb.org", "Timeweb"), ("beget.com", "Beget"),
    ("beget.pro", "Beget"), ("beget.ru", "Beget"), ("selectel.org", "Selectel"), ("selectel.ru", "Selectel"),
    ("hostinger.com", "Hostinger"), ("awsdns", "Amazon Route 53"), ("azure-dns", "Azure DNS"),
    ("googledomains.com", "Google"), ("google.com", "Google"), ("domaincontrol.com", "GoDaddy"),
    ("registrar-servers.com", "Namecheap"), ("digitalocean.com", "DigitalOcean"), ("hetzner.com", "Hetzner"),
    ("hetzner.de", "Hetzner"), ("sprinthost.net", "SprintHost"), ("jino.ru", "Jino"), ("masterhost.ru", "Masterhost"),
    ("dnsimple.com", "DNSimple"), ("ovh.net", "OVH"), ("gandi.net", "Gandi"),
)
MAIL_PROVIDERS = (
    ("mx.cloudflare.net", "Cloudflare Email Routing"), ("yandex.net", "Яндекс 360"), ("yandex.ru", "Яндекс 360"),
    ("google.com", "Google Workspace"), ("googlemail.com", "Google Workspace"), ("outlook.com", "Microsoft 365"),
    ("mail.ru", "VK WorkMail"), ("zoho.com", "Zoho"), ("zoho.eu", "Zoho"), ("protonmail.ch", "Proton"),
    ("icloud.com", "iCloud"), ("beget.com", "Beget"), ("timeweb.ru", "Timeweb"), ("reg.ru", "REG.RU"),
    ("mailgun.org", "Mailgun"), ("sendgrid.net", "SendGrid"), ("ovh.net", "OVH"), ("hostinger.com", "Hostinger"),
)


def match(host: str, table: tuple[tuple[str, str], ...]) -> str | None:
    """Имя по таблице: «mx.yandex.net» → «Яндекс 360». Шаблон с точкой — суффикс домена, без — подстрока."""
    h = host.lower().rstrip(".")
    for pattern, name in table:
        if "." in pattern:
            if h == pattern or h.endswith("." + pattern):
                return name
        elif pattern in h:
            return name
    return None


def cdn_of(as_name: str | None, cnames: list[str]) -> str | None:
    for cname in cnames:
        if found := match(cname, CDN_BY_CNAME):
            return found
    if as_name:
        low = as_name.lower()
        for pattern, name in CDN_BY_AS_NAME:
            if pattern in low:
                return name
    return None


# --- резолвер ------------------------------------------------------------------

def _server(spec: str) -> tuple[str, int]:
    spec = spec.strip()
    if spec.startswith("["):  # [::1]:5353
        host, _, rest = spec[1:].partition("]")
        return host, int(rest.lstrip(":") or 53)
    if spec.count(":") == 1:
        host, port = spec.split(":")
        return host, int(port)
    return spec, 53


@dataclass
class Resolver:
    """Обёртка над dnspython: никогда не бросает, а говорит, что случилось."""

    servers: list[str] = field(default_factory=list)
    timeout: float = 3.0

    def __post_init__(self) -> None:
        self.note = ""
        if self.servers:
            r = dns.asyncresolver.Resolver(configure=False)
            r.nameservers = [dns.nameserver.Do53Nameserver(*_server(s)) for s in self.servers]
            self.note = ", ".join(self.servers)
        else:
            try:
                r = dns.asyncresolver.Resolver()
                self.note = "системный"
            except dns.resolver.NoResolverConfiguration:
                r = dns.asyncresolver.Resolver(configure=False)
                r.nameservers = ["1.1.1.1", "8.8.8.8"]
                self.note = "1.1.1.1, 8.8.8.8 (системный не найден)"
        r.timeout = self.timeout
        r.lifetime = self.timeout * 2
        r.cache = dns.resolver.Cache()
        self._r = r

    async def query(self, name: str, rtype: str) -> tuple[list[str], str | None]:
        """(значения, ошибка). Ошибка: nxdomain | timeout | no-servers | None."""
        try:
            answer = await self._r.resolve(name, rtype, raise_on_no_answer=False)
        except dns.resolver.NXDOMAIN:
            return [], "nxdomain"
        except dns.exception.Timeout:
            return [], "timeout"
        except (dns.resolver.NoNameservers, dns.resolver.NoAnswer):
            return [], "no-servers"
        except dns.exception.DNSException as exc:
            return [], type(exc).__name__
        if answer.rrset is None:
            return [], None
        out = []
        for rr in answer.rrset:
            if rtype == "TXT":
                out.append(b"".join(rr.strings).decode("utf-8", "replace"))
            else:
                out.append(rr.to_text().rstrip("."))
        return out, None

    async def cname_chain(self, name: str, limit: int = 8) -> list[str]:
        chain, current = [], name
        for _ in range(limit):
            values, _err = await self.query(current, "CNAME")
            if not values:
                break
            current = values[0]
            chain.append(current)
        return chain

    async def ptr(self, ip: str) -> str | None:
        values, _err = await self.query(str(dns.reversename.from_address(ip)), "PTR")
        return values[0] if values else None

    async def asn(self, ip: str) -> dict[str, Any] | None:
        """Сеть адреса через Team Cymru (DNS, без ключей): AS, префикс, страна, имя сети."""
        addr = ipaddress.ip_address(ip)
        if addr.version == 4:
            q = ".".join(reversed(ip.split("."))) + ".origin.asn.cymru.com"
        else:
            q = ".".join(reversed(addr.exploded.replace(":", ""))) + ".origin6.asn.cymru.com"
        rows, _err = await self.query(q, "TXT")
        best = None
        for row in rows:  # «13335 | 104.16.0.0/13 | US | arin | 2014-03-28», самый точный префикс — главный
            parts = [p.strip() for p in row.split("|")]
            if len(parts) < 3 or not parts[0]:
                continue
            try:
                net = ipaddress.ip_network(parts[1], strict=False)
            except ValueError:
                continue
            if best is None or net.prefixlen > best[1].prefixlen:
                best = (parts, net)
        if best is None:
            return None
        parts, net = best
        asn = parts[0].split()[0]
        info = {"asn": int(asn) if asn.isdigit() else asn, "prefix": str(net), "country": parts[2],
                "registry": parts[3] if len(parts) > 3 else ""}
        names, _err = await self.query(f"AS{asn}.asn.cymru.com", "TXT")
        if names:
            as_name = names[0].split("|")[-1].strip()
            if len(as_name) > 4 and as_name[-4:-2] == ", ":  # «… Cloudflare, Inc., US» → без страны
                as_name = as_name[:-4]
            info["as_name"] = as_name
        return info


def resolver_for(run) -> Resolver:
    return Resolver(list(run.setting("recon.dns_servers") or []), float(run.setting("recon.dns_timeout")))


# --- общие шаги модулей --------------------------------------------------------------

def target_names(run, www: bool = True) -> list[str]:
    """Имена цели, которые стоит разрешить: корень, www и отдельные домены из хостов."""
    t = run.target
    if t.is_ip:
        return []
    names = [t.root]
    if www and not t.root.startswith("www."):
        names.append("www." + t.root)
    for rule in t.scope.include:
        if rule.domain and rule.domain not in names:
            names.append(rule.domain)
    return names


# Внутренние сети: из интернета туда не попасть, провайдера у них нет. Документационные
# диапазоны (192.0.2.0/24 и т.п.) Python тоже считает private — но это не «локалка».
_INTERNAL = tuple(ipaddress.ip_network(n) for n in (
    "0.0.0.0/8", "10.0.0.0/8", "100.64.0.0/10", "127.0.0.0/8", "169.254.0.0/16", "172.16.0.0/12",
    "192.168.0.0/16", "::1/128", "::/128", "fc00::/7", "fe80::/10"))


def private(ip: str) -> bool:
    addr = ipaddress.ip_address(ip)
    if addr.version == 6 and addr.ipv4_mapped:
        addr = addr.ipv4_mapped
    return any(addr in net for net in _INTERNAL if net.version == addr.version)


def org_name(as_name: str | None) -> str:
    """«CLOUDFLARENET - Cloudflare, Inc.» → «Cloudflare, Inc.»: человеку нужна компания, а не ярлык сети."""
    if not as_name:
        return ""
    handle, sep, org = as_name.partition(" - ")
    return org.strip() if sep and org.strip() else as_name


def fmt_ms(v: float) -> str:
    return "<1 мс" if v < 1 else f"{v:.0f} мс"


async def resolve_names(run, res: Resolver, names: list[str], quiet: bool = False) -> dict[str, dict[str, list]]:
    """Имена → {ip: {"domains": [...], "cnames": [...]}}; в консоль — что во что разрешилось."""
    ipmap: dict[str, dict[str, list]] = {}
    for name in names:
        cnames = await res.cname_chain(name)
        a, err_a = await res.query(name, "A")
        aaaa, _err = await res.query(name, "AAAA")
        if not quiet:
            if not a and not aaaa:
                why = {"nxdomain": "нет такого имени", "timeout": "DNS не ответил", None: "нет адресов"}.get(err_a, err_a)
                run.log(f"[-] {name}: {why}", "dim")
                continue
            run.log(f"[*] {name}")
            for c in cnames:
                run.log(f"    CNAME  {c}")
            if a:
                run.log(f"    A      {', '.join(a)}")
            if aaaa:
                run.log(f"    AAAA   {', '.join(aaaa)}")
        for ip in a + aaaa:
            entry = ipmap.setdefault(ip, {"domains": [], "cnames": []})
            if name not in entry["domains"]:
                entry["domains"].append(name)
            entry["cnames"] += [c for c in cnames if c not in entry["cnames"]]
    return ipmap


async def describe_ips(run, res: Resolver, ipmap: dict[str, dict[str, list]], quiet: bool = False) -> None:
    """Для каждого IP: сеть и провайдер, обратное имя, CDN. Всё сохраняется в находки."""
    async def one(ip: str) -> tuple[str, dict | None, str | None]:
        if private(ip):
            return ip, None, await res.ptr(ip)
        info, ptr = await asyncio.gather(res.asn(ip), res.ptr(ip))
        return ip, info, ptr

    for ip, info, ptr in await asyncio.gather(*(one(ip) for ip in ipmap)):
        entry = ipmap[ip]
        info = info or {}
        cdn = cdn_of(info.get("as_name"), entry.get("cnames", []))
        await run.emit("ip", ip, domains=entry.get("domains", []), asn=info.get("asn"), as_name=info.get("as_name"),
                       prefix=info.get("prefix"), country=info.get("country"), registry=info.get("registry"),
                       ptr=ptr, cdn=cdn, private=private(ip), version=ipaddress.ip_address(ip).version)
        if quiet:
            continue
        run.log(f"[*] {ip}")
        if info:
            run.log(f"    сеть   AS{info.get('asn')} {info.get('as_name') or '?'}  {info.get('prefix')}  {info.get('country')}")
        elif private(ip):
            run.log("    сеть   частный адрес — провайдера нет")
        else:
            run.log("    сеть   не удалось узнать", "dim")
        run.log(f"    ptr    {ptr or '—'}")
        if cdn:
            run.log(f"    [!] {cdn} — чужая инфраструктура: сайт прячется за ней, порты этого адреса не трогаем", "warn")


async def ensure_ips(run, res: Resolver) -> dict[str, dict[str, Any]]:
    """Адреса цели. Если их ещё никто не нашёл — находит сам (тихо, только DNS)."""
    def known() -> dict[str, dict[str, Any]]:
        return {a["value"]: a["data"] for a in run.assets("ip")
                if a["data"].get("domains") or run.target.scope.allows(a["value"])}

    ips = known()
    if ips:
        return ips
    run.log("[*] адреса цели ещё не известны — узнаю через DNS", "dim")
    ipmap = await resolve_names(run, res, target_names(run), quiet=True)
    if run.target.is_ip:
        ipmap.setdefault(run.target.root, {"domains": [], "cnames": []})
    for rule in run.target.scope.include:
        if rule.network is not None and rule.network.num_addresses == 1:
            ipmap.setdefault(str(rule.network.network_address), {"domains": [], "cnames": []})
    await describe_ips(run, res, ipmap, quiet=True)
    ips = known()
    run.log(f"    найдено адресов: {len(ips)}", "dim")
    return ips
