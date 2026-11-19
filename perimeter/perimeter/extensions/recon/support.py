"""Поддерживается ли ещё ветка программы: по данным endoflife.date.

Версия 8.1.2 → ветка 8.1 → исправления безопасности до 2025-12-31, последний выпуск 8.1.34,
актуальная ветка 8.5. Без поддержки новые дыры не закрывают вовсе — это хуже любой известной CVE.
"""

from __future__ import annotations

from datetime import date
from typing import Any

from . import versions

SOON_DAYS = 180  # «скоро закончится»: полгода — столько обычно занимает переход на новую ветку

STATES = {
    "ended": ("без поддержки", "err"),
    "soon": ("поддержка скоро закончится", "warn"),
    "security": ("только исправления безопасности", ""),
    "active": ("поддерживается", "ok"),
    "unknown": ("ветка неизвестна", "dim"),
}


def _parts(text: str) -> list[int] | None:
    out = []
    for piece in str(text).split("."):
        if not piece.isdigit():
            return None
        out.append(int(piece))
    return out or None


def cycle_for(cycles: list[dict[str, Any]], version: str) -> dict[str, Any] | None:
    """Ветка версии: самая длинная, с которой версия совпадает по началу (9.0.30 → «9.0», 3.6.0 → «3»)."""
    numbers = [int(t) for t in versions.tokens(version) if isinstance(t, int)]
    best = None
    for c in cycles:
        parts = _parts(str(c.get("cycle", "")))
        if parts and numbers[:len(parts)] == parts and (best is None or len(parts) > len(_parts(str(best["cycle"])))):
            best = c
    return best


def _day(value: Any) -> date | None:
    if isinstance(value, str):
        try:
            return date.fromisoformat(value[:10])
        except ValueError:
            return None
    return None


def newest(cycles: list[dict[str, Any]]) -> dict[str, Any] | None:
    """Актуальная ветка: вышедшая последней из тех, что ещё поддерживаются."""
    live = [c for c in cycles if c.get("eol") is not True and not (_day(c.get("eol")) and _day(c.get("eol")) <= date.today())]
    pool = live or cycles
    return max(pool, key=lambda c: (str(c.get("releaseDate") or ""), _parts(str(c.get("cycle"))) or []), default=None)


def status(cycles: list[dict[str, Any]], version: str, today: date | None = None) -> dict[str, Any]:
    """Всё о поддержке версии — поля для находки «технология» (eol_*)."""
    today = today or date.today()
    top = newest(cycles)
    out: dict[str, Any] = {"eol_state": "unknown", "eol_cycle": "", "eol_date": "", "eol_support": "", "eol_latest": "",
                           "eol_behind": False, "eol_newest": str(top["cycle"]) if top else "",
                           "eol_newest_latest": str(top.get("latest") or "") if top else "", "eol_lts": False, "eol_days": None}
    c = cycle_for(cycles, version)
    if c is None:
        return out
    eol, support = c.get("eol"), c.get("support")
    end = _day(eol)
    out.update(eol_cycle=str(c["cycle"]), eol_date=end.isoformat() if end else "", eol_lts=bool(c.get("lts")),
               eol_support=_day(support).isoformat() if _day(support) else "",
               eol_latest=str(c.get("latest") or ""))
    if out["eol_latest"]:
        out["eol_behind"] = versions.compare(version, out["eol_latest"]) < 0
    if eol is True or (end is not None and end <= today):
        out["eol_state"] = "ended"
    elif end is not None and (end - today).days <= SOON_DAYS:
        out["eol_state"] = "soon"
    elif _day(support) is not None and _day(support) <= today:
        out["eol_state"] = "security"
    else:
        out["eol_state"] = "active"
    if end is not None:
        out["eol_days"] = (end - today).days
    return out


def describe(info: dict[str, Any]) -> str:
    """Одной строкой для консоли и подсказок: «ветка 8.1 — без поддержки с 2025-12-31»."""
    state, cycle = info.get("eol_state"), info.get("eol_cycle")
    if state == "unknown" or not cycle:
        return "ветка не найдена в endoflife.date"
    when = info.get("eol_date")
    if state == "ended":
        text = f"ветка {cycle} — без поддержки" + (f" с {when}" if when else "")
    elif state == "soon":
        text = f"ветка {cycle} — поддержка закончится {when} (через {info.get('eol_days')} дн)"
    elif state == "security":
        text = f"ветка {cycle} — только исправления безопасности" + (f" до {when}" if when else "")
    else:
        text = f"ветка {cycle} — поддерживается" + (f" до {when}" if when else "")
    if info.get("eol_behind"):
        text += f"; в ветке уже есть {info['eol_latest']}"
    if info.get("eol_newest") and info.get("eol_newest") != cycle and state in ("ended", "soon"):
        text += f"; актуальная ветка {info['eol_newest']}" + (f" ({info['eol_newest_latest']})" if info.get("eol_newest_latest") else "")
    return text
