#!/usr/bin/env python3
"""Собрать базу «IP → страна» для всех платформ из DB-IP lite.

ЗАЧЕМ: флаг и страну хоста приложение определяет локально, не доверяя
самоотчёту. Прежняя база лежала в репозитории без генератора и устарела:
диапазоны в ней шли по стране РАСПРЕДЕЛЕНИЯ блока, а не фактического
размещения (например 107.184.0.0–107.189.191.255 целиком значился US, хотя
часть его физически в Нидерландах). Этот скрипт обновляет базу одной командой.

ИСТОЧНИК: DB-IP lite (https://db-ip.com/db/download/ip-to-country-lite),
лицензия CC BY 4.0 — обязательна ссылка на источник, см. README.

ФОРМАТ BMV2 (общий для Rust/Kotlin/Swift):
    "BMV2" + n:u32 + deltas[n]:u32 + lens[n]:u32 + cc[n]:2×ASCII,  big-endian
    start[i] = end[i-1] + delta[i]   (end[-1] = 0, сложение по модулю 2^32)
    end[i]   = start[i] + len[i]     (границы включительно)

Запуск:  python3 tools/build-ip2cc.py [ГГГГ-ММ]
"""
import gzip
import ipaddress
import pathlib
import struct
import sys
import urllib.request
from datetime import date

ROOT = pathlib.Path(__file__).resolve().parent.parent
# gz — там, где файл читается сжатым; raw — где распакованным (iOS).
TARGETS = [
    (ROOT / "apps/bmv-gui/data/ip2cc.dat", True),
    (ROOT / "apps/android/app/src/main/assets/ip2cc.dat", True),
    (ROOT / "apps/ios/BeMyVPN/Resources/ip2cc.bin", False),
]


def fetch(month: str) -> bytes:
    url = f"https://download.db-ip.com/free/dbip-country-lite-{month}.csv.gz"
    print(f"скачиваю {url}")
    # Без User-Agent сервер отдаёт 403 (умолчание urllib он не принимает).
    req = urllib.request.Request(url, headers={"User-Agent": "bemyvpn-ip2cc/1.0"})
    with urllib.request.urlopen(req, timeout=120) as r:
        return r.read()


def parse_ipv4(blob: bytes):
    """Строки CSV → отсортированные диапазоны IPv4, соседние с одной страной слиты."""
    rows = []
    for line in gzip.decompress(blob).decode().splitlines():
        lo, hi, cc = line.split(",")
        if ":" in lo:  # IPv6 приложениям пока не нужен
            continue
        # ZZ — «страна неизвестна»: так DB-IP помечает служебные и приватные
        # диапазоны (10/8, 192.168/16, 127/8, 169.254/16 …). В базу их не берём:
        # приложение должно показать глобус, а не флаг несуществующей страны.
        if len(cc) != 2 or not cc.isalpha() or cc.upper() == "ZZ":
            continue
        rows.append((int(ipaddress.IPv4Address(lo)), int(ipaddress.IPv4Address(hi)), cc.upper()))
    rows.sort()

    merged = []
    for lo, hi, cc in rows:
        if merged and merged[-1][2] == cc and lo == merged[-1][1] + 1:
            merged[-1][1] = hi  # стык с той же страной — экономим запись
        else:
            merged.append([lo, hi, cc])
    return merged


def encode(ranges) -> bytes:
    n = len(ranges)
    out = bytearray(b"BMV2")
    out += struct.pack(">I", n)
    deltas, lens, ccs = [], [], bytearray()
    prev_end = 0
    for lo, hi, cc in ranges:
        deltas.append((lo - prev_end) & 0xFFFFFFFF)
        lens.append(hi - lo)
        ccs += cc.encode("ascii")
        prev_end = hi
    out += struct.pack(f">{n}I", *deltas)
    out += struct.pack(f">{n}I", *lens)
    out += ccs
    return bytes(out)


def verify(raw: bytes, ranges):
    """Читаем обратно ТЕМ ЖЕ алгоритмом, что и приложения, и сверяем контрольные IP."""
    import bisect

    assert raw[:4] == b"BMV2"
    n = struct.unpack_from(">I", raw, 4)[0]
    p = 8
    deltas = struct.unpack_from(f">{n}I", raw, p); p += 4 * n
    lens = struct.unpack_from(f">{n}I", raw, p); p += 4 * n
    cc = raw[p:p + 2 * n]
    starts, ends, prev_end = [0] * n, [0] * n, 0
    for i in range(n):
        starts[i] = (prev_end + deltas[i]) & 0xFFFFFFFF
        ends[i] = (starts[i] + lens[i]) & 0xFFFFFFFF
        prev_end = ends[i]

    def look(ip):
        k = int(ipaddress.IPv4Address(ip))
        i = bisect.bisect_right(starts, k) - 1
        if i < 0 or k > ends[i]:
            return None
        return cc[i * 2:i * 2 + 2].decode()

    src = {lo: c for lo, _, c in ranges}
    assert look(str(ipaddress.IPv4Address(ranges[0][0]))) == ranges[0][2]
    assert look("1.1.1.1") == "AU", "контрольный IP 1.1.1.1 должен быть AU"
    assert look("8.8.8.8") == "US", "контрольный IP 8.8.8.8 должен быть US"
    assert look("10.0.0.1") is None, "приватных адресов в базе быть не должно"
    assert look("192.168.1.1") is None, "приватных адресов в базе быть не должно"
    print(f"проверка чтения: ок ({n} диапазонов, {len(src)} стартов)")


def main():
    month = sys.argv[1] if len(sys.argv) > 1 else date.today().strftime("%Y-%m")
    try:
        blob = fetch(month)
    except Exception as e:  # свежий месяц ещё не выложен — берём прошлый
        y, m = map(int, month.split("-"))
        prev = f"{y - 1}-12" if m == 1 else f"{y}-{m - 1:02d}"
        print(f"{month} недоступен ({e}), беру {prev}")
        blob = fetch(prev)

    ranges = parse_ipv4(blob)
    raw = encode(ranges)
    verify(raw, ranges)

    for path, gz in TARGETS:
        data = gzip.compress(raw, 9) if gz else raw
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        print(f"записан {path.relative_to(ROOT)}  ({len(data) / 1024:.0f} КБ{', gzip' if gz else ''})")


if __name__ == "__main__":
    main()
