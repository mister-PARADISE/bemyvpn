#!/usr/bin/env python3
"""РАСКЛАДЫВАЕТ design/palette.toml ПО ЧЕТЫРЁМ ТЕМАМ.

ЧТО ЭТО. Единственный способ поменять цвет в приложении: правишь
`design/palette.toml`, запускаешь этот скрипт — и все четыре темы (Slint, Swift,
Kotlin, Rust) переписаны из одного места. До него смена одного цвета была
правкой в четырёх файлах на четырёх языках, и они уже успели разойтись: в окне
стояло `#FFFFFF14` (0.0784) там, где на телефонах 0.08.

КОГДА ЗАПУСКАТЬ. Руками, сразу после правки источника:

    python3 design/sync-palette.py            # переписать темы
    python3 design/sync-palette.py --check    # только проверить (это делает сторож)

ПОЧЕМУ НЕ ГЕНЕРАТОР В СБОРКЕ. Генератор — это шаг сборки сразу в четырёх
тулчейнах (build.rs, фаза в Xcode, задача в Gradle) плюс сгенерированные файлы в
истории. Цветов десяток, меняются раз в месяц. Скрипт руками плюс сторож
(`apps/bmv-gui/tests/palette_is_one_source.rs`) дают почти всю пользу за малую
долю возни.

ПЕРЕПИСЫВАЕТСЯ ТОЛЬКО РАЗМЕЧЕННЫЙ УЧАСТОК каждой темы — между строками-маркерами
(см. `BEGIN`/`END` ниже). Всё остальное в этих файлах написано руками: длинные
пояснения, отступы, вылеты ореола, платформенные мелочи. Терять их нельзя, и
скрипт их не видит.

ИДЕМПОТЕНТЕН: вывод зависит только от источника, поэтому второй запуск подряд не
меняет ни байта. Проверяется тем же `--check` сразу после записи.
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "design" / "palette.toml"

BEGIN = "// ── НАЧАЛО: значения из design/palette.toml (правит design/sync-palette.py) ──"
END = "// ── КОНЕЦ: значения из design/palette.toml ──"


# ── разбор источника ─────────────────────────────────────────────────────────
# Плоский подмножественный TOML: `[секция]` и `ключ = значение  # роль`. Полный
# разбор TOML сюда не тащим: в Python 3.9 (системный на macOS) `tomllib` ещё нет,
# а ради тридцати строк заводить зависимость — больше возни, чем эти двенадцать.
def load(path):
    data, roles, section = {}, {}, None
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("["):
            section = line.strip("[]")
            data[section] = {}
            continue
        key, _, rest = line.partition("=")
        key, rest = key.strip(), rest.strip()
        # Значение сперва, роль потом: у цвета внутри кавычек свой `#`, и
        # делить строку по нему — значит отрезать сам цвет.
        if rest.startswith('"'):
            close = rest.index('"', 1)
            value, tail = rest[1:close], rest[close + 1:]
        else:
            raw, _, tail = rest.partition("#")
            value = float(raw)
        data[section][key] = value
        roles[f"{section}.{key}"] = tail.strip().lstrip("#").strip()
    return data, roles


def num(v):
    """0.30 → «0.3», 0.86 → «0.86», 1300.0 → «1300». Без хвостовых нулей."""
    return f"{v:g}"


def rgb(hexstr):
    h = hexstr.lstrip("#")
    return int(h[0:2], 16), int(h[2:4], 16), int(h[4:6], 16)


# ── отрисовка блоков ─────────────────────────────────────────────────────────
# Порядок цветов один во всех четырёх темах — так их можно сверять глазами.
ORDER = ["bg", "card", "card_hi", "tile", "float", "accent", "fg", "dim", "red", "amber"]
# Имя цвета в каждой оболочке (там, где оно отличается от ключа источника).
NAMES = {
    "slint": {"card_hi": "card-hi", "float": "float-bg"},
    "swift": {"card_hi": "cardHi"},
    "kotlin": {"card_hi": "cardHi"},
}
# Терминалу поверхности не нужны: у него их нет, кроме подсветки строки (s2).
CLI = [("ACCENT", "accent"), ("AMBER", "amber"), ("RED", "red"), ("DIM", "dim"),
       ("FG", "fg"), ("BG", "bg"), ("SEL", "card_hi")]


def pad(names):
    return max(len(n) for n in names)


# ПАРЯЩИЙ СЛОЙ раскладывается в три графические оболочки одинаковыми числами:
# длина в них меряется в своих единицах (px у Slint, pt у SwiftUI, dp у Compose),
# но на экране это одно и то же расстояние. Пересчёта здесь больше НЕТ — раньше
# он был («×2» для размытия тени), и именно он оказался неверен: замер дал вылет
# 13.3 pt на iPhone против 8.5 px в окне при «одинаковых» числах. Тень убрана,
# см. `[float]` в источнике.
#
# Терминалу этот раздел не достаётся: у него нет ни поверхностей, ни длин.
def render_slint(d, roles):
    c, e, m, t, f = d["colors"], d["edges"], d["mix"], d["timing"], d["float"]
    names = [NAMES["slint"].get(k, k) for k in ORDER]
    w = pad(names)
    out = [
        f"out property <color> {n + ':':{w + 1}} {c[k]};".ljust(46) + f"// {roles['colors.' + k]}"
        for n, k in zip(names, ORDER)
    ]
    out += [
        "",
        f"out property <color> hairline: #FFFFFF.with-alpha({num(e['hairline'])});",
        f"out property <color> hairline-float: #FFFFFF.with-alpha({num(e['hairline_float'])});",
        f"public pure function edge(tint: color, hover: bool) -> color "
        f"{{ return tint.with-alpha(hover ? {num(e['edge_bright'])} : {num(e['edge'])}); }}",
        f"public pure function edge-soft(tint: color) -> color "
        f"{{ return tint.with-alpha({num(e['edge_soft'])}); }}",
        f"public pure function edge-done(tint: color) -> color "
        f"{{ return tint.with-alpha({num(e['edge_done'])}); }}",
        f"public pure function disc-fill(tint: color) -> color "
        f"{{ return tint.with-alpha({num(e['disc_fill'])}); }}",
        f"public pure function disc-ring(tint: color) -> color "
        f"{{ return tint.with-alpha({num(e['disc_ring'])}); }}",
        "",
        # `mix(a, f)` в Slint отдаёт f СВОЕГО цвета и (1−f) чужого — отсюда 1−k.
        f"public pure function picked(tint: color) -> color "
        f"{{ return root.card.mix(tint, {num(1 - m['picked'])}); }}",
        f"public pure function touched(tint: color) -> color "
        f"{{ return root.card.mix(tint, {num(1 - m['touched'])}); }}",
        "",
        f"out property <length> float-radius: {num(f['radius'])}px;".ljust(46)
        + f"// {roles['float.radius']}",
        f"out property <length> veil: {num(f['veil'])}px;".ljust(46)
        + f"// {roles['float.veil']}",
        "",
        f"out property <duration> copied-ms: {num(t['copied_ms'])}ms;",
    ]
    return out


def render_swift(d, roles):
    c, e, m, t, f = d["colors"], d["edges"], d["mix"], d["timing"], d["float"]
    names = [NAMES["swift"].get(k, k) for k in ORDER]
    w = pad(names)
    out = [
        f"static let {n:{w}} = Color(hex: 0x{c[k].lstrip('#')})".ljust(46) + f"// {roles['colors.' + k]}"
        for n, k in zip(names, ORDER)
    ]
    out += [
        "",
        f"static let hairline = Color.white.opacity({num(e['hairline'])})",
        f"static let hairlineFloat = Color.white.opacity({num(e['hairline_float'])})",
        "static func edge(_ tint: Color = accent, bright: Bool = false) -> Color "
        f"{{ tint.opacity(bright ? {num(e['edge_bright'])} : {num(e['edge'])}) }}",
        f"static func edgeSoft(_ tint: Color = accent) -> Color {{ tint.opacity({num(e['edge_soft'])}) }}",
        f"static func edgeDone(_ tint: Color = accent) -> Color {{ tint.opacity({num(e['edge_done'])}) }}",
        f"static func discFill(_ tint: Color) -> Color {{ tint.opacity({num(e['disc_fill'])}) }}",
        f"static func discRing(_ tint: Color) -> Color {{ tint.opacity({num(e['disc_ring'])}) }}",
        "",
        f"static func picked(_ tint: Color = accent) -> Color {{ mix(card, tint, {num(m['picked'])}) }}",
        f"static func touched(_ tint: Color = accent) -> Color {{ mix(card, tint, {num(m['touched'])}) }}",
        "",
        f"static let floatRadius: CGFloat = {num(f['radius'])}".ljust(46)
        + f"// {roles['float.radius']}",
        f"static let veil: CGFloat = {num(f['veil'])}".ljust(46)
        + f"// {roles['float.veil']}",
        "",
        f"static let copiedMs: TimeInterval = {num(t['copied_ms'] / 1000)}",
    ]
    return out


def render_kotlin(d, roles):
    c, e, m, t, f = d["colors"], d["edges"], d["mix"], d["timing"], d["float"]
    names = [NAMES["kotlin"].get(k, k) for k in ORDER]
    w = pad(names)
    out = [
        f"val {n:{w}} = Color(0xFF{c[k].lstrip('#')})".ljust(46) + f"// {roles['colors.' + k]}"
        for n, k in zip(names, ORDER)
    ]
    out += [
        "",
        f"val hairline = Color.White.copy(alpha = {num(e['hairline'])}f)",
        f"val hairlineFloat = Color.White.copy(alpha = {num(e['hairline_float'])}f)",
        "fun edge(tint: Color = accent, bright: Boolean = false): Color = "
        f"tint.copy(alpha = if (bright) {num(e['edge_bright'])}f else {num(e['edge'])}f)",
        f"fun edgeSoft(tint: Color = accent): Color = tint.copy(alpha = {num(e['edge_soft'])}f)",
        f"fun edgeDone(tint: Color = accent): Color = tint.copy(alpha = {num(e['edge_done'])}f)",
        f"fun discFill(tint: Color): Color = tint.copy(alpha = {num(e['disc_fill'])}f)",
        f"fun discRing(tint: Color): Color = tint.copy(alpha = {num(e['disc_ring'])}f)",
        "",
        f"fun picked(tint: Color = accent): Color = mix(tint, {num(m['picked'])}f)",
        f"fun touched(tint: Color = accent): Color = mix(tint, {num(m['touched'])}f)",
        "",
        f"val floatRadius = {num(f['radius'])}.dp".ljust(46)
        + f"// {roles['float.radius']}",
        f"val veil = {num(f['veil'])}.dp".ljust(46) + f"// {roles['float.veil']}",
        "",
        f"const val COPIED_MS = {num(t['copied_ms'])}L",
    ]
    return out


def render_rust(d, roles):
    c = d["colors"]
    w = pad([n for n, _ in CLI])
    return [
        f"const {n}: Color = Color::Rgb(0x{r:02X}, 0x{g:02X}, 0x{b:02X});".ljust(52)
        + f"// {c[k]} — {roles['colors.' + k]}"
        for n, k in CLI
        for r, g, b in [rgb(c[k])]
    ]


TARGETS = [
    ("apps/bmv-gui/ui/theme.slint", "    ", render_slint),
    ("apps/ios/BeMyVPN/Theme.swift", "    ", render_swift),
    ("apps/android/app/src/main/java/org/bemyvpn/Theme.kt", "    ", render_kotlin),
    ("apps/bmv-cli/src/tui.rs", "", render_rust),
]


def block(indent, lines):
    body = "\n".join((indent + ln).rstrip() for ln in lines)
    return f"{indent}{BEGIN}\n{body}\n{indent}{END}"


def apply(text, indent, lines, rel):
    """Меняет ТОЛЬКО участок между маркерами. Нет маркеров — это ошибка, а не
    повод переписать файл целиком."""
    i = j = None
    for n, ln in enumerate(text.splitlines()):
        if ln.strip() == BEGIN:
            i = n
        elif ln.strip() == END:
            j = n
    if i is None or j is None or j < i:
        sys.exit(f"{rel}: не нашёл маркеров участка — вставь их или почини скрипт")
    src = text.splitlines(keepends=True)
    return "".join(src[:i]) + block(indent, lines) + "\n" + "".join(src[j + 1:])


def main():
    check = "--check" in sys.argv
    data, roles = load(SRC)
    bad = []
    for rel, indent, render in TARGETS:
        p = ROOT / rel
        was = p.read_text(encoding="utf-8")
        now = apply(was, indent, render(data, roles), rel)
        if was == now:
            continue
        if check:
            bad.append(rel)
        else:
            p.write_text(now, encoding="utf-8")
            print(f"переписан {rel}")
    if check and bad:
        print("ТЕМА РАЗОШЛАСЬ С design/palette.toml: " + ", ".join(bad), file=sys.stderr)
        if len(bad) > 1:
            print("(разошлась не одна — значит оболочки разошлись и между собой)", file=sys.stderr)
        print("почини одной командой: python3 design/sync-palette.py", file=sys.stderr)
        return 1
    if check:
        print("все четыре темы совпадают с источником")
    elif not bad:
        print("темы уже совпадали с источником — менять нечего")
    return 0


if __name__ == "__main__":
    sys.exit(main())
