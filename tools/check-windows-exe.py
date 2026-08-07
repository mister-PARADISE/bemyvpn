#!/usr/bin/env python3
"""Сторож готового Windows-exe: архитектура и таблица импортов.

ПРОВЕРЯЕТ РЕЗУЛЬТАТ, А НЕ НАМЕРЕНИЕ. Настройка `crt-static` в .cargo/config.toml
может быть правильной, а флаг — не доехать до линкера (чужая секция, RUSTFLAGS
из окружения, переезд на другую цель). Единственное честное доказательство —
сам файл: если рантайм MSVC вшит, в таблице импортов НЕТ ни vcruntime140.dll,
ни api-ms-win-crt-*.dll. Иначе на чистой Windows человек получает системное
окно «не удаётся продолжить выполнение кода» — ровно это и случилось с
bemyvpn-windows-arm64.exe, когда правило крт-статика было прописано только для
цели x86_64 (см. .cargo/config.toml).

Заодно сверяет архитектуру из заголовка PE с обещанной в имени файла: ARM-раннер
отвечает «x86_64» на uname, и однажды ARM-сборка уже уехала в релиз под чужим
именем.

Разбор PE своими руками — сознательно: dumpbin.exe лежит внутри Visual Studio и
на PATH раннера его нет, а python есть на обоих (windows-latest и windows-11-arm).

Запуск:  python tools/check-windows-exe.py <файл.exe> [x86_64|ARM64]

Вывод ТОЛЬКО латиницей: консоль Windows роняет шаг на кириллице
(UnicodeEncodeError) — проверка отработала бы верно, а задача упала бы на
сообщении о её успехе.
"""

import struct
import sys

# Части рантайма MSVC/UCRT. Любая из них в импортах = exe требует Visual C++
# Redistributable на машине пользователя.
CRT_MARKERS = ("vcruntime", "msvcp", "msvcr", "ucrtbase", "api-ms-win-crt")

MACHINES = {0x8664: "x86_64", 0xAA64: "ARM64", 0x14C: "x86"}


def rva_to_off(sections, rva):
    """Виртуальный адрес → смещение в файле (по таблице секций)."""
    for va, vsize, rawsize, raw in sections:
        if va <= rva < va + max(vsize, rawsize):
            return rva - va + raw
    raise ValueError("RVA 0x%x is outside every section" % rva)


def cstr(data, off):
    end = data.index(b"\0", off)
    return data[off:end].decode("ascii", "replace")


def parse(path):
    """→ (архитектура, отсортированный список импортируемых DLL)."""
    with open(path, "rb") as f:
        d = f.read()

    pe = struct.unpack_from("<I", d, 0x3C)[0]
    if d[pe:pe + 4] != b"PE\0\0":
        raise ValueError("not a PE file: missing PE signature")
    machine, nsections = struct.unpack_from("<HH", d, pe + 4)
    opt_size = struct.unpack_from("<H", d, pe + 20)[0]

    opt = pe + 24
    magic = struct.unpack_from("<H", d, opt)[0]
    # PE32+ (0x20b) держит на 16 байт больше до каталогов, чем PE32 (0x10b).
    dirs = opt + (112 if magic == 0x20B else 96)
    import_rva, import_size = struct.unpack_from("<II", d, dirs + 8)  # каталог №1

    sections = []
    for i in range(nsections):
        s = pe + 24 + opt_size + i * 40
        vsize, va, rawsize, raw = struct.unpack_from("<IIII", d, s + 8)
        sections.append((va, vsize, rawsize, raw))

    dlls = []
    if import_rva and import_size:
        off = rva_to_off(sections, import_rva)
        # IMAGE_IMPORT_DESCRIPTOR — по 20 байт, конец = запись из одних нулей.
        while d[off:off + 20] != b"\0" * 20:
            name_rva = struct.unpack_from("<I", d, off + 12)[0]
            dlls.append(cstr(d, rva_to_off(sections, name_rva)))
            off += 20

    return MACHINES.get(machine, hex(machine)), sorted(dlls, key=str.lower)


def main():
    if len(sys.argv) < 2:
        print("usage: check-windows-exe.py <file.exe> [x86_64|ARM64]")
        return 2

    path = sys.argv[1]
    want_arch = sys.argv[2] if len(sys.argv) > 2 else None
    arch, dlls = parse(path)

    print("=== %s ===" % path)
    print("PE arch: %s" % arch)
    print("imports (%d):" % len(dlls))
    for name in dlls:
        print("  %s" % name)

    bad = []
    if want_arch and arch != want_arch:
        bad.append("PE arch is %s, expected %s" % (arch, want_arch))

    crt = [n for n in dlls if any(m in n.lower() for m in CRT_MARKERS)]
    if crt:
        bad.append("depends on the MSVC runtime DLLs: %s -- crt-static did not "
                   "reach the linker, see .cargo/config.toml" % ", ".join(crt))
    # Ноль импортов = разбор ушёл не туда; лучше упасть, чем зря успокоить.
    if not dlls:
        bad.append("no imports at all -- the import table was not parsed")

    if bad:
        for line in bad:
            print("FAIL: %s" % line)
        return 1

    print("OK: static CRT, no runtime DLL required")
    return 0


if __name__ == "__main__":
    sys.exit(main())
