"""Версии программ: сравнение и проверка «попадает ли версия под CVE».

Версии бывают какими угодно: 1.24.0, 9.6p1 (OpenSSH), 1.0.2k (OpenSSL), 2.0.0-rc1, 9.4.43.v20210629
(Jetty). Сравниваем по частям: числа как числа, буквы как буквы. Пред-релизы (alpha, beta, rc, dev)
младше релиза, а «хвосты» вроде p1 или k — старше: 9.6p1 > 9.6, 1.0.2k > 1.0.2.

В NVD уязвимые версии записаны в CPE (cpe:2.3:a:openbsd:openssh:9.6:p1:…) — точная версия плюс
«update», — или диапазоном: versionStartIncluding / versionEndExcluding и т. п.
"""

from __future__ import annotations

import re
from functools import cmp_to_key
from typing import Any

_TOKEN = re.compile(r"\d+|[a-z]+")
# пред-релизы — раньше релиза; чем меньше число, тем раньше
_PRE = {"dev": -6, "snapshot": -6, "alpha": -5, "a": -5, "beta": -4, "b": -4, "pre": -3, "preview": -3,
        "m": -3, "milestone": -3, "rc": -2, "cr": -2}


def tokens(version: str) -> list[int | str]:
    v = (version or "").lower().strip().lstrip("v")
    v = v.split("+", 1)[0]  # +build — не влияет на порядок
    return [int(t) if t.isdigit() else t for t in _TOKEN.findall(v)]


def _cmp_token(x: int | str, y: int | str) -> int:
    if x == y:
        return 0
    if isinstance(x, int) and isinstance(y, int):
        return -1 if x < y else 1
    if isinstance(x, str) and isinstance(y, str):
        px, py = _PRE.get(x), _PRE.get(y)
        if px is not None or py is not None:  # rc против p1: пред-релиз младше
            return -1 if (px if px is not None else 1) < (py if py is not None else 1) else 1
        return -1 if x < y else 1
    # число против буквы на одном месте (9.6.1 против 9.6p1): буква — «хвост», младше следующего числа
    return 1 if isinstance(x, int) else -1


def compare(a: str, b: str) -> int:
    """-1, 0, 1 — как a относится к b. compare('9.6p1', '9.6') == 1, compare('2.0.0-rc1', '2.0.0') == -1."""
    ta, tb = tokens(a), tokens(b)
    for x, y in zip(ta, tb):
        c = _cmp_token(x, y)
        if c:
            return c
    if len(ta) == len(tb):
        return 0
    longer, sign = (ta[len(tb):], 1) if len(ta) > len(tb) else (tb[len(ta):], -1)
    if all(t == 0 for t in longer):  # 1.24 == 1.24.0
        return 0
    first = longer[0]
    if isinstance(first, str) and first in _PRE:  # 2.0.0rc1 < 2.0.0
        return -sign
    return sign


def sort_key():
    return cmp_to_key(compare)


def precise(version: str) -> bool:
    """Достаточно ли версии, чтобы сверять с уязвимостями: «10» (только мажор) — нет, «10.1.2» — да."""
    nums = [t for t in tokens(version) if isinstance(t, int)]
    return len(nums) >= 2


# --- CPE ---------------------------------------------------------------------------

def split_cpe(cpe: str) -> list[str]:
    """cpe:2.3:a:joomla:joomla\\!:4.0.0:… → поля; двоеточие внутри поля экранировано обратным слешем."""
    parts, cur, esc = [], [], False
    for ch in cpe:
        if esc:
            cur.append(ch)
            esc = False
        elif ch == "\\":
            cur.append(ch)
            esc = True
        elif ch == ":":
            parts.append("".join(cur))
            cur = []
        else:
            cur.append(ch)
    parts.append("".join(cur))
    return parts + ["*"] * (13 - len(parts)) if len(parts) < 13 else parts


def compact_match(m: dict[str, Any]) -> dict[str, Any]:
    """Из cpeMatch NVD — только то, что нужно для проверки версии."""
    f = split_cpe(m["criteria"])
    out: dict[str, Any] = {"vp": f"{f[3]}:{f[4]}", "v": f[5], "u": f[6], "tsw": f[10]}
    for key, short in (("versionStartIncluding", "si"), ("versionStartExcluding", "se"),
                       ("versionEndIncluding", "ei"), ("versionEndExcluding", "ee")):
        if m.get(key):
            out[short] = m[key]
    return out


def _same_release(version: str, cpe_version: str, update: str) -> bool:
    """Точная версия из CPE: «9.6» + update «p1» — это 9.6p1; update «*» — любая сборка 9.6."""
    if update not in ("*", "-", ""):
        return compare(version, cpe_version + update) == 0 or compare(version, f"{cpe_version}-{update}") == 0
    if compare(version, cpe_version) == 0:
        return True
    if update == "*" and version.lower().startswith(cpe_version.lower()):
        rest = version[len(cpe_version):]
        return bool(rest) and rest[0].isalpha() and not tokens(rest)[0] in _PRE  # 9.6p1 — это сборка 9.6
    return False


def affected(m: dict[str, Any], version: str) -> tuple[bool, str]:
    """(уязвима ли версия, как поняли): 'exact' — версия названа прямо, 'range' — попала в диапазон,
    'all' — уязвимы все версии (диапазона нет)."""
    v, u = m.get("v", "*"), m.get("u", "*")
    if v not in ("*", "-"):
        return _same_release(version, v, u), "exact"
    if v == "-":
        return False, ""
    bounds = [k for k in ("si", "se", "ei", "ee") if m.get(k)]
    if not bounds:
        return True, "all"
    if "si" in m and compare(version, m["si"]) < 0:
        return False, ""
    if "se" in m and compare(version, m["se"]) <= 0:
        return False, ""
    if "ei" in m and compare(version, m["ei"]) > 0:
        return False, ""
    if "ee" in m and compare(version, m["ee"]) >= 0:
        return False, ""
    return True, "range"


def fixed_in(matches: list[dict[str, Any]], version: str) -> str:
    """С какой версии исправлено: наименьший versionEndExcluding из диапазонов, куда попала версия.
    Если известен только «уязвимы до X включительно» — «после X»."""
    exclusive = [m["ee"] for m in matches if m.get("ee") and affected(m, version)[0]]
    if exclusive:
        return min(exclusive, key=sort_key())
    inclusive = [m["ei"] for m in matches if m.get("ei") and affected(m, version)[0]]
    if inclusive:
        return "после " + max(inclusive, key=sort_key())
    return ""
