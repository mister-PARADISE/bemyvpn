"""Хосты цели: что именно входит в цель и куда модулям можно ходить.

Это главный предохранитель панели. Активный модуль сможет обратиться только
к хосту, который `Scope.allows()` пропустит, — проверку делает ядро, а не
автор модуля.

Правила:
* корневой домен разрешает сам себя и все свои поддомены;
* в дополнительных записях можно указать домены, `*.домен`, IP и подсети (CIDR);
* исключения важнее всего остального;
* похожие имена не проходят: `evil-example.com` не входит в `example.com`.
"""

from __future__ import annotations

import ipaddress
import re
from dataclasses import dataclass, field
from urllib.parse import urlsplit

import idna

IPNetwork = ipaddress.IPv4Network | ipaddress.IPv6Network
_LABEL = re.compile(r"^(?!-)[a-z0-9-]{1,63}(?<!-)$")


class ScopeError(ValueError):
    pass


def normalize_host(value: str, *, strip_www: bool = False) -> str:
    """URL или кириллический домен → чистое имя хоста в ASCII.

    normalize_host('https://WWW.Пример.рф:443/путь', strip_www=True) → 'xn--e1afmkfd.xn--p1ai'

    `strip_www` нужен только для корня цели: «www.site.ru» и «site.ru» — один
    сайт. В правилах и проверках www не трогаем: исключение «www.site.ru»
    не должно превратиться в исключение всего домена.
    """
    raw = (value or "").strip()
    if not raw:
        raise ScopeError("Пустой адрес")
    try:  # голый IPv6: «2001:db8::1» urlsplit прочитал бы как хост «2001»
        return str(ipaddress.ip_address(raw.strip("[]")))
    except ValueError:
        pass
    if "://" not in raw:
        raw = "//" + raw
    try:
        host = urlsplit(raw).hostname or ""
    except ValueError:
        raise ScopeError(f"Не похоже на адрес: {value!r}") from None
    host = host.strip().rstrip(".").lower()
    if not host:
        raise ScopeError(f"Не похоже на адрес: {value!r}")
    try:
        return str(ipaddress.ip_address(host))
    except ValueError:
        pass
    if not host.isascii():
        # IDNA 2008 (UTS 46, как в браузерах): «straße.de» — это xn--strae-oqa.de, а не чужой strasse.de
        try:
            host = idna.encode(host, uts46=True, transitional=False).decode("ascii")
        except (idna.IDNAError, UnicodeError):
            raise ScopeError(f"Недопустимое доменное имя: {value!r}") from None
    if strip_www and host.startswith("www.") and host.count(".") >= 2:
        host = host[4:]
    labels = host.split(".")
    if len(labels) < 2 or not all(_LABEL.match(label) for label in labels):
        raise ScopeError(f"Недопустимое доменное имя: {value!r}")
    if labels[-1].isdigit():
        raise ScopeError(f"Недопустимое доменное имя: {value!r}")
    return host


def display_host(host: str) -> str:
    """ASCII (punycode) → как читает человек: xn--e1afmkfd.xn--p1ai → пример.рф"""
    if "xn--" not in host:
        return host
    try:
        return idna.decode(host)
    except (idna.IDNAError, UnicodeError):
        return host


# Общие домены: внутри них сайты разных владельцев. Целью может быть конкретный сайт
# (shop.github.io), но не весь общий домен — иначе в цель попали бы чужие сайты.
SHARED_SUFFIXES = frozenset({
    "github.io", "gitlab.io", "netlify.app", "vercel.app", "pages.dev", "workers.dev", "herokuapp.com",
    "web.app", "firebaseapp.com", "appspot.com", "azurewebsites.net", "cloudfront.net", "blogspot.com",
    "myshopify.com", "wixsite.com", "tilda.ws", "ucoz.ru", "narod.ru", "beget.tech", "timeweb.cloud",
    "co.uk", "org.uk", "ac.uk", "gov.uk", "com.ru", "net.ru", "org.ru", "pp.ru", "msk.ru", "spb.ru",
    "com.ua", "org.ua", "pp.ua", "in.ua", "com.kz", "org.kz", "com.by", "co.jp", "com.au", "com.br",
    "com.tr", "com.cn", "co.in", "co.za", "co.nz", "com.mx", "com.ar",
})
MIN_PREFIX = {4: 16, 6: 48}  # шире /16 (65 536 адресов) — почти наверняка опечатка, а не свой сервер


def check_include(rule: "ScopeRule") -> None:
    """Правило, которое расширяет цель, не должно захватывать чужое."""
    if rule.network is not None and rule.network.prefixlen < MIN_PREFIX[rule.network.version]:
        raise ScopeError(f"Слишком широкая сеть {rule.network}: не шире /{MIN_PREFIX[rule.network.version]}")
    if rule.domain is not None and rule.domain in SHARED_SUFFIXES:
        raise ScopeError(f"{rule.domain} — общий домен, в нём сайты разных владельцев; укажите свой поддомен")


def is_ip(value: str) -> bool:
    try:
        ipaddress.ip_address(value)
        return True
    except ValueError:
        return False


@dataclass(frozen=True)
class ScopeRule:
    raw: str
    domain: str | None = None
    network: IPNetwork | None = None

    @classmethod
    def parse(cls, value: str) -> "ScopeRule":
        text = value.strip()
        if not text:
            raise ScopeError("Пустая запись")
        if "/" in text and "://" not in text:
            try:
                return cls(text, network=ipaddress.ip_network(text, strict=False))
            except ValueError:
                pass
        if text.startswith("*."):
            text = text[2:]
        host = normalize_host(text)
        if is_ip(host):
            return cls(value.strip(), network=ipaddress.ip_network(host))
        return cls(value.strip(), domain=host)

    def matches(self, host: str) -> bool:
        if self.network is not None:
            if not is_ip(host):
                return False
            return ipaddress.ip_address(host) in self.network
        assert self.domain is not None
        return host == self.domain or host.endswith("." + self.domain)

    def canonical(self) -> str:
        """Единый вид для хранения: «https://API.example.com/x» → «api.example.com», «10.0.0.0/8», «10.0.0.5»."""
        if self.network is not None:
            net = self.network
            return str(net.network_address) if net.num_addresses == 1 else str(net)
        return self.domain or self.raw

    def label(self) -> str:
        if self.network is not None:
            net = self.network
            return str(net.network_address) if net.num_addresses == 1 else str(net)
        return f"{display_host(self.domain or '')} и поддомены"


@dataclass(frozen=True)
class Scope:
    root: str
    include: tuple[ScopeRule, ...] = field(default=())
    exclude: tuple[ScopeRule, ...] = field(default=())

    @classmethod
    def build(cls, root: str, include: list[str] | None = None, exclude: list[str] | None = None) -> "Scope":
        return cls(
            normalize_host(root, strip_www=True),
            tuple(ScopeRule.parse(x) for x in include or [] if x.strip()),
            tuple(ScopeRule.parse(x) for x in exclude or [] if x.strip()),
        )

    def rules(self) -> tuple[ScopeRule, ...]:
        root = ScopeRule(self.root, network=ipaddress.ip_network(self.root)) if is_ip(self.root) \
            else ScopeRule(self.root, domain=self.root)
        return (root, *self.include)

    def explain(self, value: str) -> tuple[bool, str]:
        """(разрешено?, почему) — для подсказки в интерфейсе."""
        try:
            host = normalize_host(value)
        except ScopeError as exc:
            return False, str(exc)
        for rule in self.exclude:
            if rule.matches(host):
                return False, f"Исключено правилом «{rule.raw}»"
        for rule in self.rules():
            if rule.matches(host):
                return True, f"Входит в «{rule.label()}»"
        return False, "не входит в цель"

    def allows(self, value: str) -> bool:
        return self.explain(value)[0]


def parse_rules(lines: list[str], include: bool = False) -> tuple[list[str], list[str]]:
    """Проверяет список правил из формы: (нормальные строки, ошибки). include — правила, расширяющие цель."""
    good, errors = [], []
    for line in lines:
        try:
            rule = ScopeRule.parse(line)
            if include:
                check_include(rule)
            canon = rule.canonical()
            if canon not in good:
                good.append(canon)
        except ScopeError as exc:
            errors.append(f"{line.strip()}: {exc}")
    return good, errors
