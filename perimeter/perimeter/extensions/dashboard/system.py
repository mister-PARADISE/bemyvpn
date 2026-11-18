"""Снимок машины для «Обзора»: процессор, память, диск, нагрузка — как в htop.

Строки отдаются уже готовыми (полоса из «|», подпись, оттенок), поэтому
первая отрисовка на сервере и обновление из JS выглядят одинаково.
Без psutil показывается то, что умеет стандартная библиотека.
"""

from __future__ import annotations

import os
import platform
import shutil
import time
from pathlib import Path
from typing import Any

try:
    import psutil
except ImportError:  # pragma: no cover — psutil есть в зависимостях, но панель не должна падать без него
    psutil = None

from ...web.ui import plural

BAR = 30
STARTED = time.time()

if psutil is not None:
    psutil.cpu_percent(None)  # первый замер всегда 0 — делаем его заранее


def bar(pct: float, width: int = BAR) -> str:
    filled = max(0, min(width, round(pct / 100 * width)))
    return "|" * filled + " " * (width - filled)


def tone(pct: float) -> str:
    return "ok" if pct < 60 else "warn" if pct < 85 else "err"


def size(num: float) -> str:
    for unit in ("Б", "КБ", "МБ", "ГБ", "ТБ"):
        if num < 1024 or unit == "ТБ":
            return f"{num:.0f} {unit}" if unit in ("Б", "КБ") else f"{num:.1f} {unit}"
        num /= 1024
    return f"{num:.1f} ТБ"


def duration(seconds: float) -> str:
    seconds = int(seconds)
    days, rest = divmod(seconds, 86400)
    hours, rest = divmod(rest, 3600)
    minutes = rest // 60
    if days:
        return f"{days} {plural(days, 'день', 'дня', 'дней')} {hours} ч"
    if hours:
        return f"{hours} ч {minutes} мин"
    return f"{minutes} мин"


def _row(key: str, pct: float | None, text: str) -> dict[str, Any]:
    if pct is None:
        return {"key": key, "bar": None, "tone": "", "text": text}
    return {"key": key, "bar": bar(pct), "tone": tone(pct), "text": text, "pct": round(pct, 1)}


def snapshot(data_dir: Path) -> dict[str, Any]:
    rows: list[dict[str, Any]] = []
    cores = os.cpu_count() or 1
    load = os.getloadavg() if hasattr(os, "getloadavg") else None

    if psutil is not None:
        cpu = psutil.cpu_percent(None)
        rows.append(_row("cpu", cpu, f"{cpu:.0f}%  {cores} {plural(cores, 'ядро', 'ядра', 'ядер')}"))
        vm = psutil.virtual_memory()
        used = vm.total - vm.available
        rows.append(_row("ram", vm.percent, f"{size(used)} из {size(vm.total)}"))
        swap = psutil.swap_memory()
        if swap.total:
            rows.append(_row("swap", swap.percent, f"{size(swap.used)} из {size(swap.total)}"))
    elif load:
        cpu = min(100.0, load[0] / cores * 100)
        rows.append(_row("cpu", cpu, f"~{cpu:.0f}% по нагрузке  {cores} ядер"))

    disk = shutil.disk_usage(data_dir)
    disk_pct = disk.used / disk.total * 100 if disk.total else 0
    rows.append(_row("disk", disk_pct, f"{size(disk.used)} из {size(disk.total)}"))

    if load:
        rows.append(_row("load", None, "  ".join(f"{x:.2f}" for x in load)))
    if psutil is not None:
        rows.append(_row("up", None, duration(time.time() - psutil.boot_time())))

    panel = f"работает {duration(time.time() - STARTED)}"
    if psutil is not None:
        panel = f"{size(psutil.Process().memory_info().rss)} ОЗУ, {panel}"
    rows.append(_row("panel", None, panel))

    return {
        "rows": rows,
        "host": platform.node() or "localhost",
        "os": f"{platform.system()} {platform.release()}".strip(),
        "psutil": psutil is not None,
    }
