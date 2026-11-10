#!/usr/bin/env python3
"""
tdb — анализ и очистка текстовых баз: CSV, TSV, TXT с разделителями.

Запуск
    python tdb.py                    открыть и перетащить файл базы в окно — больше ничего не нужно
    python tdb.py base.csv           то же, файл открыт сразу (можно перетащить на сам скрипт)
    python tdb.py base.csv -a        анализ — заполненность и уникальность — и выход
    python tdb.py base.csv -o new.csv --drop 3,7-12 --dedupe
    python tdb.py -h                 все опции командной строки

Что умеет
    · файл базы достаточно перетащить в окно; последние базы открываются по номеру;
    · сам определяет кодировку, разделитель, кавычки и строку заголовков;
    · заполненность: значение считается данными, только если в нём есть хотя бы
      одна буква или цифра и это не маркер пустоты (NULL, \\N …);
    · уникальность: по каждому столбцу, по связке столбцов и по строкам целиком;
    · экспорт в НОВЫЙ файл: выбор и порядок столбцов, удаление дублей, пустых
      и битых строк, чистка значений, смена кодировки и разделителя.

Столбцы нумеруются с единицы и задаются как угодно: 1 3-5 email addr*
Ctrl+C во время прохода останавливает его, в меню — возвращает на шаг назад.

Исходный файл никогда не изменяется. Нужен только Python 3.8+, без библиотек.
"""

import argparse
import codecs
import csv
import io
import itertools
import json
import math
import os
import re
import shutil
import signal
import sys
import textwrap
import time
import unicodedata
from array import array
from collections import Counter
from itertools import compress, islice
from operator import itemgetter, mul, not_, or_
from pathlib import Path
from urllib.parse import unquote

__version__ = "1.0"


# ══════════════════════════════════════════════════════════════════════════════
#  НАСТРОЙКИ — то, что имеет смысл править под себя
# ══════════════════════════════════════════════════════════════════════════════

DEFAULT_FILE = r""            # файл, который открывать сразу; пусто — спросить (файл перетаскивают в окно)

# Маркеры пустоты. Значение, целиком равное одному из них (регистр и пробелы по
# краям не важны), считается пустым. Кроме того, пустым считается всё, в чём нет
# ни одной буквы и ни одной цифры: "", " ", "-", "???" …
NULL_MARKERS = ("NULL", r"\N", "N", "None", "nan", "N/A", "0000-00-00", "0000-00-00 00:00:00")

IGNORE_CASE = False           # сравнивать значения без учёта регистра и пробелов по краям
REPAIR_ROWS = False           # чинить строки: короткие дополнять, пустой «хвост» отрезать
SKIP_INITIAL_SPACE = True     # пробелы сразу после разделителя — не часть значения
DOUBLE_QUOTE = True           # две кавычки подряд внутри кавычек — это одна кавычка
FIELD_LIMIT_MB = 16           # поле длиннее — запись нечитаема (защита от непарных кавычек)
MEMORY_LIMIT_MB = 1024        # память на точный подсчёт уникальных; не хватило — оценка со знаком «≈»
OUTPUT_SUFFIX = "_clean"      # имя нового файла: base.csv → base_clean.csv
CLEAR_SCREEN = True           # каждый раздел — на чистом экране; False — сплошной лентой
RECENT_FILES = 9              # сколько последних баз помнить (список — ~/.tdb_recent); 0 — не помнить


# ══════════════════════════════════════════════════════════════════════════════
#  ВНУТРЕННИЕ КОНСТАНТЫ
# ══════════════════════════════════════════════════════════════════════════════

SEP = "\x1f"                  # чем склеиваются поля, когда строка сравнивается целиком
MASK64 = (1 << 64) - 1
SAMPLE = 256 * 1024           # сколько байт читать, чтобы определить формат
BLOCK_CELLS = 65536           # значений в одном блоке строк: больше — данные не помещаются в кэш процессора
BLOCK_BYTES = 1 << 20         # … и не больше примерно стольких байт текста
COUNTER_LIMIT = 50_000        # до стольких разных значений столбец считается точным счётчиком
SET_LIMIT = 262_144           # до стольких — обычным множеством, дальше — компактной таблицей
TOP_KEEP = 20                 # сколько частых значений хранить на столбец
PRUNE_AT = 2_000_000          # отдельный проход: при стольких разных значениях редкие отбрасываются
DATA_SUFFIXES = (".csv", ".tsv", ".txt", ".tab", ".dat", ".psv")
RECENT_NAME = ".tdb_recent"   # файл со списком недавних баз — в домашней папке


# ══════════════════════════════════════════════════════════════════════════════
#  ТЕРМИНАЛ: цвет, ширина, числа, ввод
# ══════════════════════════════════════════════════════════════════════════════

class UI:
    interactive = False       # работаем в меню, а не по опциям командной строки
    color = False             # красить вывод
    wipe = ""                 # чем очищать экран: "ansi", "command" или "" — нечем
    live = False              # терминал настоящий: прогресс рисуется на месте, одной строкой
    steps = False             # терминала нет (консоль IDE): прогресс — редкими отдельными строками


def setup_console():
    for stream in (sys.stdout, sys.stderr):
        try:
            stream.reconfigure(errors="replace")
        except (AttributeError, ValueError):
            pass
    ansi = True
    if os.name == "nt":       # консоль Windows понимает ANSI только после включения режима
        ansi = False
        try:
            import ctypes
            kernel = ctypes.windll.kernel32
            for std in (-11, -12):
                handle, mode = kernel.GetStdHandle(std), ctypes.c_uint32()
                if kernel.GetConsoleMode(handle, ctypes.byref(mode)):
                    ansi = bool(kernel.SetConsoleMode(handle, mode.value | 0x0004)) or ansi
        except Exception:
            pass
    env = os.environ
    idle = "idlelib" in sys.modules       # оболочка IDLE: ни цвета, ни возврата каретки
    terminal = sys.stdout.isatty() and not idle and env.get("TERM") != "dumb"
    capable = terminal and ansi or "PYCHARM_HOSTED" in env
    UI.color = bool(env.get("FORCE_COLOR")) or capable and "NO_COLOR" not in env
    UI.live = sys.stderr.isatty() and not idle
    if terminal and CLEAR_SCREEN:
        UI.wipe = "ansi" if ansi else "command"


def _paint(code):
    return lambda text: f"\x1b[{code}m{text}\x1b[0m" if UI.color and text != "" else f"{text}"


dim, bold, accent, green, yellow, red = (_paint(code) for code in ("90", "1", "36", "32", "33", "31"))


def term_width():
    return max(64, min(shutil.get_terminal_size((100, 30)).columns - 1, 150))


def term_height():
    return shutil.get_terminal_size((100, 30)).lines


def wlen(text):
    """Ширина строки на экране: широкие иероглифы — две клетки, комбинируемые знаки — ноль."""
    if text.isascii():
        return len(text)
    return sum(0 if unicodedata.combining(ch) else 2 if unicodedata.east_asian_width(ch) in "WF" else 1
               for ch in text)


def clip(text, width):
    """Обрезает строку до ширины width, ставя многоточие."""
    if wlen(text) <= width:
        return text
    out, used = [], 0
    for ch in text:
        used += wlen(ch)
        if used > width - 1:
            break
        out.append(ch)
    return "".join(out) + "…" if width > 0 else ""


def clip_left(text, width):
    """Длинный путь обрезается слева: конец пути важнее начала."""
    if wlen(text) <= width:
        return text
    out, used = [], 1
    for ch in reversed(text):
        used += wlen(ch)
        if used > width:
            break
        out.append(ch)
    return "…" + "".join(reversed(out))


def pad(text, width, right=False):
    text = clip(text, width)
    gap = " " * (width - wlen(text))
    return gap + text if right else text + gap


_UNSAFE = re.compile("[\x00-\x1f\x7f-\x9f\u200b-\u200f\u2028-\u202e\u2060-\u2069\ufeff]")


def show(value):
    """Значение из файла → одна безопасная для терминала строка (без управляющих символов)."""
    return _UNSAFE.sub(" ", value)


def num(n):
    return f"{n:,}".replace(",", " ")


def roughly(n):
    """Округление до трёх значащих цифр — чтобы оценка не выглядела точной."""
    n = int(n)
    if n < 1000:
        return n
    step = 10 ** (len(str(n)) - 3)
    return int(round(n / step) * step)


def short(n):
    if n >= 1_000_000:
        return f"{n / 1_000_000:.1f} млн"
    if n >= 10_000:
        return f"{n / 1000:.0f} тыс"
    return num(n)


def pct(part, whole):
    """Процент с одним знаком; 100 и 0 показываются, только когда они настоящие."""
    if not whole:
        return "—"
    value = 100.0 * part / whole
    if part and value < 0.05:
        return "<0.1"
    if part < whole and value >= 99.95:
        return ">99.9"
    return f"{value:.1f}"


def size_text(n):
    for unit in ("Б", "КБ", "МБ", "ГБ"):
        if n < 1024:
            return f"{n:.0f} {unit}" if unit == "Б" else f"{n:.1f} {unit}"
        n /= 1024
    return f"{n:.1f} ТБ"


def time_text(seconds):
    if seconds < 10:
        return f"{seconds:.2f} с" if seconds < 1 else f"{seconds:.1f} с"
    seconds = int(round(seconds))
    if seconds < 60:
        return f"{seconds} с"
    minutes, seconds = divmod(seconds, 60)
    if minutes < 60:
        return f"{minutes} мин {seconds:02d} с"
    hours, minutes = divmod(minutes, 60)
    return f"{hours} ч {minutes:02d} мин"


def plural(n, one, few, many):
    n = abs(n) % 100
    if 10 < n < 20:
        return many
    n %= 10
    return one if n == 1 else few if 1 < n < 5 else many


def repeats_text(total, unique, exact):
    """Сколько повторов: total − unique. Для оценки (HyperLogLog) малые числа — шум, их не показываем."""
    extra = max(0, total - unique)
    if exact:
        return num(extra)
    return "≈ " + num(roughly(extra)) if extra * 50 > total else "меньше 2 %"


def meter(part, width=10):
    """Полоска доли. Не ноль — хотя бы одна клетка; до конца — только ровно 100 %."""
    cells = int(round(part * width))
    if part > 0 and cells == 0:
        cells = 1
    if part < 1 and cells == width:
        cells -= 1
    rest = width - cells                  # без цвета фон полоски рисуется точками, иначе его не отличить
    return accent("▄" * cells) + (dim("▄" * rest) if UI.color else "·" * rest)


def reason(error):
    """Ошибка человеческими словами."""
    if isinstance(error, FileNotFoundError):
        return f"файл не найден: {error.filename}"
    if isinstance(error, PermissionError):
        return f"нет доступа (файл открыт в другой программе или защищён): {error.filename}"
    if isinstance(error, OSError) and error.strerror:
        return f"{error.strerror}: {error.filename}" if error.filename else error.strerror
    return str(error)


class Back(Exception):
    """Ctrl+C в подсказке: шаг назад."""


class Dropped(Exception):
    """В подсказку перетащили файл: его надо открыть, на каком бы экране мы ни были."""


def ask(label="", drop=True):
    """Ввод строки. drop=False — там, где путь к файлу и есть ожидаемый ответ."""
    print()
    try:
        text = input(f"  {label}› ").strip()
    except KeyboardInterrupt:
        print()
        raise Back from None
    except EOFError:
        print()
        raise SystemExit(0)
    if drop and UI.interactive and len(text) > 2:
        try:                              # полный путь к существующему файлу — значит, перетащили базу
            path = clean_path(text)
            if path.is_absolute() and path.is_file():
                raise Dropped(text)
        except (OSError, ValueError):
            pass
    return text


_LAYOUT = str.maketrans("йцукенгшщзфывапролдячсмить", "qwertyuiopasdfghjklzxcvbnm")


def key(text):
    """Команда из одной буквы работает в любой раскладке: «ы» — это «s»."""
    text = text.strip().lower()
    return text.translate(_LAYOUT) if len(text) == 1 else text


def confirm(question):
    return ask(f"{question} [д/Н] ").lower() in ("д", "да", "y", "yes")


def clear_screen():
    """Чистый лист: и экран, и прокрутка. Где очищать нечем (консоль IDE, вывод в файл) — экраны идут подряд."""
    if UI.wipe == "ansi":
        sys.stdout.write("\x1b[H\x1b[2J\x1b[3J")
        sys.stdout.flush()
    elif UI.wipe:
        os.system("cls" if os.name == "nt" else "clear")


def menu(rows):
    """rows — (клавиша, название, значение[, выделить значение[, приглушённая приписка]]).
    Что не помещается в ширину окна, обрезается, а не переносится."""
    width = max(wlen(row[1]) for row in rows)
    room = term_width() - width - 8
    for row in rows:
        text = clip(row[2], room)
        line = text if len(row) > 3 and row[3] else dim(text)
        left = room - wlen(text) - 3
        if len(row) > 4 and row[4] and left > 4:
            line += "   " + dim(clip(row[4], left))
        print(f"  {accent(row[0])}  {pad(row[1], width)}   {line}".rstrip())


def hints(*pairs):
    """Строка подсказок «клавиша — действие»; длинная переносится."""
    width, line, used = term_width() - 2, [], 0
    for keys, text in pairs:
        size = wlen(keys) + 3 + wlen(text)
        if line and used + 5 + size > width:
            print("  " + dim("  ·  ").join(line))
            line, used = [], 0
        used += size + (5 if line else 0)
        line.append(f"{keys} {dim('— ' + text)}")
    print("  " + dim("  ·  ").join(line))


def say(text, paint=str):
    """Строка текста с отступом; длинная переносится по словам, а не там, где кончилось окно."""
    for line in textwrap.wrap(text, term_width() - 2) or [""]:
        print("  " + paint(line))


def note(text):
    say(text, dim)


def alert(text):
    say(text, yellow)


def fail(text):
    say(text, red)


class Progress:
    """Строка прогресса в stderr. Появляется, только если проход длится дольше трети секунды."""

    def __init__(self, label, total):
        self.label, self.total = label, max(total, 1)
        self.began = self.drawn = time.perf_counter()
        self.width = 0

    def update(self, done, rows):
        now = time.perf_counter()
        if now - self.drawn < (0.1 if UI.live else 5) or now - self.began < 0.3 or not (UI.live or UI.steps):
            return
        self.drawn = now
        part = min(done / self.total, 1.0)
        speed = done / (now - self.began)
        left = (self.total - done) / speed if speed else 0
        if not UI.live:                   # консоль без возврата каретки — просто строка раз в несколько секунд
            print(dim(f"  {self.label} · {part * 100:.0f} % · {short(rows)} строк · ещё {time_text(left)}"), flush=True)
            return
        room = term_width() - wlen(self.label) - 30
        text = ""
        pieces = f"{part * 100:3.0f} %", f"{short(rows)} строк", f"{size_text(speed)}/с", f"ещё {time_text(left)}"
        for piece in pieces:
            if wlen(text) + wlen(piece) + 3 > room:
                break
            text += ("   " if text else "") + piece
        visible = wlen(self.label) + 30 + wlen(text)
        tail = " " * max(0, self.width - visible)
        sys.stderr.write(f"\r  {self.label}  {meter(part, 24)}  {dim(text)}{tail}")
        sys.stderr.flush()
        self.width = visible

    def close(self):
        if self.width:
            sys.stderr.write("\r" + " " * self.width + "\r")
            sys.stderr.flush()
            self.width = 0


# ══════════════════════════════════════════════════════════════════════════════
#  ПРАВИЛА: что считать пустым и как сравнивать значения
# ══════════════════════════════════════════════════════════════════════════════

_ALNUM = None


def alnum():
    """Все буквы и цифры Юникода одним множеством.

    frozenset.isdisjoint(строка) — самый быстрый способ узнать, есть ли в строке хотя бы
    одна буква или цифра: работает на C и останавливается на первом же попадании.
    """
    global _ALNUM
    if _ALNUM is None:
        _ALNUM = frozenset(filter(str.isalnum, map(chr, range(0x40000))))
    return _ALNUM


def _spellings(word):
    """Все варианты регистра слова: NULL, null, Null, nuLL …"""
    pairs = [(ch.lower(), ch.upper()) if ch.lower() != ch.upper() and len(ch.lower() + ch.upper()) == 2 else (ch,)
             for ch in word]
    if sum(len(pair) > 1 for pair in pairs) > 10:
        return {word, word.lower(), word.upper(), word.capitalize()}
    return {"".join(variant) for variant in itertools.product(*pairs)}


class Rules:
    """Правила разбора. Заполнено — значит, есть буква или цифра и это не маркер пустоты."""

    def __init__(self, markers=NULL_MARKERS, soft=IGNORE_CASE, repair=REPAIR_ROWS):
        self.markers = tuple(dict.fromkeys(m.strip() for m in markers if m.strip()))
        self.soft = soft              # сравнивать без учёта регистра и пробелов по краям
        self.repair = repair          # чинить строки с не тем числом полей
        self.nulls = frozenset(v for m in self.markers for v in _spellings(m))
        self.no_data = alnum().isdisjoint

    def signature(self):
        return self.markers, self.soft, self.repair

    def empty_flags(self, values):
        """Для каждого значения: True — пусто. Работает целиком на C."""
        flags = list(map(self.no_data, values))
        if self.nulls:
            flags = list(map(or_, flags, map(self.nulls.__contains__, map(str.strip, values))))
        return flags

    def filled(self, value):
        return not self.no_data(value) and value.strip() not in self.nulls

    def keys(self, values):
        """Значения в том виде, в каком они сравниваются между собой."""
        return map(str.casefold, map(str.strip, values)) if self.soft else values

    def row_keys(self, rows):
        if self.soft:
            return (SEP.join(map(str.strip, row)).casefold() for row in rows)
        return map(SEP.join, rows)


# ══════════════════════════════════════════════════════════════════════════════
#  ПОДСЧЁТ РАЗНЫХ ЗНАЧЕНИЙ
# ══════════════════════════════════════════════════════════════════════════════

class HashSet64:
    """Точное множество 64-битных хешей: открытая адресация в array('Q'), 8 байт на ячейку.

    В 4–5 раз компактнее set() — на нём держатся подсчёт уникальных и поиск дублей в больших
    файлах. Ноль означает пустую ячейку, поэтому нулевой хеш не хранится и не считается.
    """

    __slots__ = ("cells", "mask", "count", "limit")

    def __init__(self, expect=0):
        size = 1 << 12
        while size * 3 // 10 < expect:      # с запасом: сразу после создания расти не придётся
            size <<= 1
        self.count = 0
        self._allocate(size)

    def _allocate(self, size):
        self.cells = array("Q", [0]) * size
        self.mask = size - 1
        self.limit = size * 3 // 5          # заполнение не выше 60 %

    def _grow(self):
        old = self.cells
        self._allocate(len(old) * 2)
        cells, mask = self.cells, self.mask
        for h in filter(None, old):
            i = h & mask
            while cells[i]:
                i = (i + 1) & mask
            cells[i] = h

    def add_many(self, hashes):
        cells, mask, count, limit = self.cells, self.mask, self.count, self.limit
        for h in hashes:
            i = h & mask
            v = cells[i]
            while v != h:
                if not v:
                    cells[i] = h
                    count += 1
                    if count > limit:
                        self._grow()
                        cells, mask, limit = self.cells, self.mask, self.limit
                    break
                i = (i + 1) & mask
                v = cells[i]
        self.count = count

    def add_flags(self, hashes):
        """Для каждого хеша: True — встретился впервые (и запомнен). Нулевой хеш всегда «новый»."""
        cells, mask, count, limit = self.cells, self.mask, self.count, self.limit
        flags = []
        push = flags.append
        for h in hashes:
            i = h & mask
            v = cells[i]
            while v != h:
                if not v:
                    cells[i] = h
                    count += 1
                    if count > limit:
                        self._grow()
                        cells, mask, limit = self.cells, self.mask, self.limit
                    push(True)
                    break
                i = (i + 1) & mask
                v = cells[i]
            else:
                push(not h)
        self.count = count
        return flags


class HyperLogLog:
    """Оценка числа разных значений в 64 КБ памяти, ошибка около ±0.4 %.

    Формула — по Эртлю (arXiv:1702.01284): без таблиц поправок и без скачка на малых числах.
    """

    P = 16

    def __init__(self):
        self.cells = bytearray(1 << self.P)

    def add_many(self, hashes):
        cells, low, shift, top = self.cells, (1 << self.P) - 1, self.P, 65 - self.P
        for h in hashes:
            i = h & low
            rank = top - (h >> shift).bit_length()
            if rank > cells[i]:
                cells[i] = rank

    def count(self):
        m, q = len(self.cells), 64 - self.P
        histogram = Counter(self.cells)
        z = m * self._tau(1 - histogram[q + 1] / m)
        for rank in range(q, 0, -1):
            z = (z + histogram[rank]) * 0.5
        z += m * self._sigma(histogram[0] / m)
        return 0 if math.isinf(z) else int(round(m * m / (2 * math.log(2)) / z))

    @staticmethod
    def _sigma(x):
        if x == 1:
            return math.inf
        y, z = 1.0, x
        while True:
            x *= x
            previous = z
            z += x * y
            y += y
            if previous == z:
                return z

    @staticmethod
    def _tau(x):
        if x in (0, 1):
            return 0.0
        y, z = 1.0, 1 - x
        while True:
            x = math.sqrt(x)
            previous = z
            y *= 0.5
            z -= (1 - x) ** 2 * y
            if previous == z:
                return z / 3


class Distinct:
    """Счётчик разных значений. Три яруса: set (быстро, пока значений немного) →
    HashSet64 (точно и компактно) → HyperLogLog (оценка, когда памяти не хватило)."""

    __slots__ = ("small", "table", "sketch")

    def __init__(self):
        self.small, self.table, self.sketch = set(), None, None

    def add(self, keys):
        if self.small is not None:
            self.small.update(map(hash, keys))
            if len(self.small) > SET_LIMIT:
                self.shrink()
        elif self.table is not None:
            self.table.add_many(map(MASK64.__and__, map(hash, keys)))
        else:
            self.sketch.add_many(map(MASK64.__and__, map(hash, keys)))

    def cost(self):
        """Примерный расход памяти в байтах."""
        if self.small is not None:
            return 80 * len(self.small)
        if self.table is not None:
            return 8 * len(self.table.cells)
        return len(self.sketch.cells)

    def shrink(self):
        """Переход на ярус компактнее. False — ужимать больше нечего."""
        if self.small is not None:
            self.table = HashSet64(len(self.small))
            self.table.add_many(map(MASK64.__and__, self.small))
            self.small = None
        elif self.table is not None:
            self.sketch = HyperLogLog()
            self.sketch.add_many(filter(None, self.table.cells))
            self.table = None
        else:
            return False
        return True

    @property
    def exact(self):
        return self.sketch is None

    def count(self):
        if self.small is not None:
            return len(self.small)
        return self.table.count if self.table is not None else self.sketch.count()


# ══════════════════════════════════════════════════════════════════════════════
#  ФОРМАТ ФАЙЛА: кодировка, разделитель, заголовок
# ══════════════════════════════════════════════════════════════════════════════

_ASCII = bytes(range(128))
_ACCENTED = re.compile(rb"(?<=[A-Za-z])[\x80-\xff](?![\x80-\xff])|(?<![\x80-\xff])[\x80-\xff](?=[A-Za-z])")
_RU_BY_FREQUENCY = "оеаинтсрвлкмдпуяыьгзбчйхжшюцщэфъё"
ENCODING_NAMES = {"utf-8": "UTF-8", "utf-8-sig": "UTF-8 с BOM", "cp1251": "Windows-1251", "cp866": "DOS-866",
                  "koi8-r": "KOI8-R", "cp1252": "Windows-1252", "utf-16": "UTF-16", "utf-32": "UTF-32"}


def encoding_name(name):
    return ENCODING_NAMES.get(name.lower(), name.upper())


def delimiter_name(char):
    return {"\t": "табуляция", " ": "пробел"}.get(char, f"«{char}»")


def detect_encoding(head, more=(), whole=True):
    """Кодировка по началу файла (head) и кускам из середины (more); whole — head и есть весь файл."""
    if head.startswith(codecs.BOM_UTF8):
        return "utf-8-sig"
    if head.startswith((codecs.BOM_UTF32_LE, codecs.BOM_UTF32_BE)):
        return "utf-32"
    if head.startswith((codecs.BOM_UTF16_LE, codecs.BOM_UTF16_BE)):
        return "utf-16"
    even, odd = head[0:65536:2].count(0), head[1:65536:2].count(0)
    pairs = min(len(head), 65536) // 2                    # UTF-16 без BOM: нулевые байты — строго через один
    if max(even, odd) * 5 > pairs and min(even, odd) * 50 < pairs:
        return "utf-16-le" if odd > even else "utf-16-be"
    pieces = (head, *more)
    high = sum(len(piece.translate(None, _ASCII)) for piece in pieces)
    if not high:
        return "utf-8"                                    # чистый ASCII
    broken = sum(piece.decode("utf-8", "replace").count("\ufffd") for piece in pieces)
    torn = 4 * len(more) + (0 if whole else 1)            # куски режут символы по краям — это не ошибки
    if broken <= high * 0.02 + torn:                      # а редкие битые байты UTF-8 не отменяют
        return "utf-8"
    blob = b"".join(pieces)
    if len(_ACCENTED.findall(blob)) * 2 > high:           # одиночные не-ASCII буквы внутри латинских слов
        return "cp1252"                                   # (José, Köln) — западноевропейский текст
    best, best_score = "cp1252", 0                        # кириллица: где больше частых русских букв
    for name in ("cp1251", "cp866", "koi8-r"):
        text = blob.decode(name, "replace").lower()
        score = sum(text.count(ch) * (len(_RU_BY_FREQUENCY) - rank) for rank, ch in enumerate(_RU_BY_FREQUENCY))
        if score > best_score:
            best, best_score = name, score
    return best


def _consistency(text, **dialect):
    """Насколько ровно текст делится на столбцы: (доля строк с самым частым числом полей, это число)."""
    try:
        rows = list(islice(csv.reader(io.StringIO(text, newline=""), **dialect), 400))
    except csv.Error:
        return 0.0, 1
    sizes = Counter(len(row) for row in rows if row)
    if not sizes:
        return 0.0, 1
    width, hits = sizes.most_common(1)[0]
    return hits / sum(sizes.values()), width


def detect_dialect(text):
    """Разделитель, символ кавычек и экранирования — по первым строкам файла."""
    base = dict(skipinitialspace=SKIP_INITIAL_SPACE, doublequote=DOUBLE_QUOTE)
    delimiter, best = ",", 0.0
    for candidate in ",;\t|:":
        share, width = _consistency(text, delimiter=candidate, **base)
        score = share * share * width if width > 1 and share >= 0.5 else 0.0
        if score > best:
            delimiter, best = candidate, score
    quote = '"'
    single = text.count(delimiter + "'") + text.count("'" + delimiter)
    double = text.count(delimiter + '"') + text.count('"' + delimiter)
    if single > 2 * double and single >= 10:
        with_single = _consistency(text, delimiter=delimiter, quotechar="'", **base)
        if with_single > _consistency(text, delimiter=delimiter, **base):
            quote = "'"
    escape = ""
    if "\\" + quote in text:
        plain = _consistency(text, delimiter=delimiter, quotechar=quote, **base)
        escaped = _consistency(text, delimiter=delimiter, quotechar=quote, escapechar="\\", **base)
        if escaped[0] > plain[0]:
            escape = "\\"
    return delimiter, quote, escape


_DATA_LIKE = re.compile(r"[+-]?\d[\d\s.,:/()+-]*|\S+@\S+\.\S+|https?://\S+", re.I)


def looks_like_header(rows):
    """Первая строка — имена столбцов или уже данные?"""
    if len(rows) < 2:
        return False
    first, body = [cell.strip() for cell in rows[0]], rows[1:201]
    if any(_DATA_LIKE.fullmatch(cell) for cell in first):   # число, дата, почта — это уже данные
        return False
    for i, name in enumerate(first):
        if name and any(len(row) > i and row[i].strip() == name for row in body):
            return False                                  # «имя» встречается среди значений — это значение
    return True


def checked_encoding(text):
    try:
        return codecs.lookup(text.strip()).name
    except LookupError:
        raise ValueError(f"неизвестная кодировка «{text.strip()}»") from None


def checked_delimiter(text):
    words = {"tab": "\t", "\\t": "\t", "таб": "\t", "space": " ", "пробел": " "}
    text = words.get(text.strip().lower(), text.strip() or text)
    if len(text) != 1:
        raise ValueError("разделитель — это один символ (или слово tab)")
    return text


def checked_count(text):
    if not text.strip().isdigit() or not int(text):
        raise ValueError("нужно целое число больше нуля")
    return int(text)


def one_char(text):
    """Символ кавычек или экранирования: «-» и «none» — нет такого."""
    return "" if text.strip().lower() in ("-", "none", "нет") else text.strip()[:1]


# ══════════════════════════════════════════════════════════════════════════════
#  ИСХОДНЫЙ ФАЙЛ И ПРОХОД ПО НЕМУ
# ══════════════════════════════════════════════════════════════════════════════

_NOT_TEXT = ((b"PK\x03\x04", "архив ZIP или документ Excel/Word (.xlsx, .docx)"),
             (b"\xd0\xcf\x11\xe0", "документ старого Excel/Word (.xls, .doc)"),
             (b"\x1f\x8b", "архив GZIP"), (b"7z\xbc\xaf\x27\x1c", "архив 7z"), (b"Rar!", "архив RAR"),
             (b"SQLite format 3\x00", "база SQLite"), (b"%PDF", "документ PDF"))


def not_text(head, encoding):
    """Если файл — не текст (архив, Excel, база SQLite, двоичные данные), возвращает, что это; иначе — пусто."""
    for magic, name in _NOT_TEXT:
        if head.startswith(magic):
            return name
    if not encoding.startswith(("utf-16", "utf-32")) and head.count(0) * 10 > len(head):
        return "двоичный файл"
    return ""


class Source:
    """Исходный файл и его формат. Что не задано вручную (forced) — определяется по содержимому."""

    def __init__(self, path, forced=None):
        self.path = Path(path)
        info = self.path.stat()
        if not self.path.is_file():
            raise OSError(f"это не файл: {self.path}")
        self.size, self.mtime = info.st_size, info.st_mtime
        self.forced = dict(forced or {})
        self.sniff()

    def sniff(self):
        forced = self.forced
        with open(self.path, "rb") as file:
            head = file.read(SAMPLE)
            more = []
            if self.size > SAMPLE * 4:
                for part in (0.4, 0.8):
                    file.seek(int(self.size * part))
                    more.append(file.read(SAMPLE // 2))
        self.encoding = forced.get("encoding") or detect_encoding(head, more, self.size <= len(head))
        alien = not_text(head, self.encoding)
        if alien:
            raise ValueError(f"это {alien}, а нужен текстовый файл: CSV, TSV, TXT")
        text = head.decode(self.encoding, "replace").lstrip("\ufeff")
        if self.size > len(head):                         # последняя строка образца оборвана
            text = text[: max(text.rfind("\n"), text.rfind("\r")) + 1] or text
        delimiter, quote, escape = detect_dialect(text)
        self.delimiter = forced.get("delimiter") or delimiter
        self.quote = forced["quote"] if "quote" in forced else quote
        self.escape = forced["escape"] if "escape" in forced else escape
        self.lineterm = "\r\n" if "\r\n" in text else "\n"

        try:
            rows = [row for row in islice(csv.reader(io.StringIO(text, newline=""), **self.dialect()), 2000)
                    if any(row)]
        except csv.Error:
            rows = []
        self.has_header = forced["header"] if "header" in forced else looks_like_header(rows)
        header = [show(cell).strip() for cell in rows[0]] if self.has_header and rows else []
        sizes = Counter(len(row) for row in (rows[1:] if self.has_header else rows))
        usual = sizes.most_common(1)[0][0] if sizes else len(header)
        if "ncols" in forced:
            self.ncols = forced["ncols"]
        elif header and (not sizes or sizes[len(header)] * 5 >= sum(sizes.values())):
            self.ncols = len(header)
        else:
            self.ncols = usual
        self.note = ""
        if header and len(header) != self.ncols:
            self.note = (f"в заголовке {len(header)} {plural(len(header), 'имя', 'имени', 'имён')}, "
                         f"а полей в строках — {self.ncols}")
        self.names = (header + [""] * self.ncols)[: self.ncols]

    def dialect(self):
        """Параметры для csv.reader."""
        options = dict(delimiter=self.delimiter, skipinitialspace=SKIP_INITIAL_SPACE)
        if self.quote:
            options.update(quotechar=self.quote, doublequote=DOUBLE_QUOTE)
        else:
            options.update(quotechar=None, quoting=csv.QUOTE_NONE)
        if self.escape:
            options["escapechar"] = self.escape
        return options

    def signature(self):
        return (str(self.path), self.size, self.mtime, self.encoding, self.delimiter, self.quote, self.escape,
                self.has_header, self.ncols)

    def label(self, index):
        return self.names[index] or f"столбец {index + 1}"

    def labels(self, indices, glue=" + "):
        return glue.join(self.label(i) for i in indices)


class Scan:
    """Один проход по файлу: отдаёт данные блоками строк, рисует прогресс, считает битое."""

    def __init__(self, src, rules, label):
        self.src, self.repair = src, rules.repair
        self.raw = open(src.path, "rb", buffering=1 << 20)
        self.text = io.TextIOWrapper(self.raw, encoding=src.encoding, errors="replace", newline="")
        self.reader = csv.reader(self.text, **src.dialect())
        self.progress = Progress(label, src.size)
        self.began = time.perf_counter()
        self.records = 0                  # записей прочитано (заголовок не в счёт)
        self.offset = 0                   # … из них до текущего блока
        self.head_lines = 0               # строк файла до первой записи с данными
        self.short = self.long = self.unreadable = self.repaired = 0
        self.samples = []                 # примеры битых строк: (номер записи, поля)
        self.swallowed = 0                # строк файла, оказавшихся внутри чужих значений (см. _suspect)
        self.interrupted = False          # нажат Ctrl+C: проход остановлен на границе блока
        self._handler = None

    def __enter__(self):
        try:                              # Ctrl+C не рвёт блок посередине, а просит остановиться
            self._handler = signal.signal(signal.SIGINT, self._on_interrupt)
        except ValueError:                # не главный поток — остаётся обычное поведение
            pass
        return self

    def __exit__(self, *exc):
        if self._handler is not None:
            signal.signal(signal.SIGINT, self._handler)
        self.progress.close()
        self.text.close()

    def _on_interrupt(self, *_):
        if self.interrupted:              # второй Ctrl+C — прервать немедленно
            raise KeyboardInterrupt
        self.interrupted = True

    def _records(self):
        """Записи файла. Нечитаемая запись (поле больше предела) не обрывает проход."""
        while True:
            try:
                yield from self.reader
                return
            except csv.Error:
                self.unreadable += 1

    def blocks(self):
        records = self._records()
        if self.src.has_header:
            next(filter(any, records), None)              # заголовок — первая непустая запись
        self.head_lines = self.reader.line_num
        tell = self.raw.tell
        most = max(16, min(BLOCK_CELLS // max(self.src.ncols, 1), 32768))
        size, last = min(most, 256), tell()
        while not self.interrupted:
            line = self.reader.line_num
            block = list(islice(records, size))
            if not block:
                return
            if self.reader.line_num - line > len(block) and self.src.ncols > 2:
                self._suspect(block)      # в блоке есть значения в несколько строк
            here = tell()                 # размер блока подстраивается под длину строк
            size = int(min(most, max(16, BLOCK_BYTES * len(block) / max(here - last, 1))))
            last = here
            self.progress.update(here, self.records)
            self.offset = self.records
            self.records += len(block)
            yield block

    def _suspect(self, block):
        """Признак непарной кавычки: значение вобрало в себя несколько строк файла,
        и эти строки сами похожи на записи — в них столько же разделителей."""
        need, delimiter = self.src.ncols - 1, self.src.delimiter
        for row in block:
            if "\n" in SEP.join(row):
                for cell in row:
                    if "\n" in cell:
                        self.swallowed += sum(line.count(delimiter) >= need for line in cell.split("\n")[1:])

    def shape(self, block):
        """Делит блок на годные строки и битые — те, где не то число полей."""
        width = self.src.ncols
        if set(map(len, block)) == {width}:               # обычный случай: весь блок ровный
            return block, ()
        good, bad = [], []
        for number, row in enumerate(block, self.offset + 1):
            size = len(row)
            if size == width:
                good.append(row)
            elif not any(row):                            # пустая строка файла
                continue
            elif self.repair and size < width:
                row.extend([""] * (width - size))
                good.append(row)
                self.repaired += 1
            elif self.repair and not any(row[width:]):    # лишние поля пусты — отрезаем
                del row[width:]
                good.append(row)
                self.repaired += 1
            else:
                if size < width:
                    self.short += 1
                else:
                    self.long += 1
                bad.append(row)
                if len(self.samples) < 5:
                    self.samples.append((number, row))
        return good, bad

    @property
    def part(self):
        """Доля прочитанного файла."""
        return min(self.raw.tell() / self.src.size, 1.0) if self.src.size else 1.0

    @property
    def multiline(self):
        """Сколько переводов строки оказалось внутри значений (в кавычках)."""
        return max(0, self.reader.line_num - self.head_lines - self.records)

    @property
    def seconds(self):
        return time.perf_counter() - self.began


def records(src):
    """Записи файла по одной — для просмотра. Нечитаемая запись приходит как None."""
    with open(src.path, "r", encoding=src.encoding, errors="replace", newline="") as file:
        reader = csv.reader(file, **src.dialect())
        skip = src.has_header
        while True:
            try:
                for row in reader:
                    if skip:                              # заголовок — первая непустая запись
                        skip = not any(row)
                    else:
                        yield row
                return
            except csv.Error:
                yield None


# ══════════════════════════════════════════════════════════════════════════════
#  АНАЛИЗ: заполненность и уникальность
# ══════════════════════════════════════════════════════════════════════════════

_KINDS = (
    ("число", re.compile(r"[+-]?\d+(?:[.,]\d+)?")),
    ("дата", re.compile(r"\d{4}[-./]\d{1,2}[-./]\d{1,2}(?:[T ]\d{1,2}:\d\d(?::\d\d(?:\.\d+)?)?)?(?:Z|[+-]\d\d:?\d\d)?"
                        r"|\d{1,2}[-./]\d{1,2}[-./]\d{2,4}(?: \d{1,2}:\d\d(?::\d\d)?)?")),
    ("почта", re.compile(r"[^@\s]+@[^@\s]+\.[^@\s.]+")),
    ("телефон", re.compile(r"\+\d[\d() -]{8,18}\d|\d?\s?\(\d{3,5}\)[\d -]{5,12}\d")),
    ("ссылка", re.compile(r"(?:https?://|www\.)\S+", re.I)),
    ("ip", re.compile(r"(?:\d{1,3}\.){3}\d{1,3}")),
    ("hex", re.compile(r"[0-9a-fA-F]{24,}")),
)


def guess_kind(sample):
    """На что похожи значения столбца. Пусто — обычный текст или вперемешку."""
    votes = Counter()
    for value in sample:
        value = value.strip()
        for name, pattern in _KINDS:
            if pattern.fullmatch(value):
                votes[name] += 1
                break
    if not votes:
        return ""
    name, hits = votes.most_common(1)[0]
    return name if hits >= 0.9 * len(sample) else ""


class Column:
    """Итог по одному столбцу."""

    def __init__(self, index, name):
        self.index, self.name = index, name
        self.filled = self.unique = 0
        self.exact = True                 # unique посчитано точно
        self.len_min = self.len_max = 0
        self.len_avg = 0.0
        self.kind = ""
        self.top = None                   # [(значение, сколько)] по убыванию; None — не считались
        self.top_exact = True
        self.empties = []                 # [(значение, сколько)] — что сочтено пустым
        self.examples = []

    def as_dict(self, rows):
        return dict(number=self.index + 1, name=self.name, filled=self.filled,
                    filled_percent=round(100 * self.filled / rows, 4) if rows else 0.0,
                    unique=self.unique, unique_exact=self.exact,
                    unique_percent=round(100 * self.unique / self.filled, 4) if self.filled else 0.0,
                    kind=self.kind, length_min=self.len_min, length_max=self.len_max,
                    length_avg=round(self.len_avg, 2), top=self.top, top_exact=self.top_exact,
                    empty_values=self.empties, examples=self.examples)


class _Tally:
    """Накопитель по столбцу. Пока разных значений немного — точный счётчик (быстро и даёт
    частые значения); когда их много — только итоги и счётчик разных."""

    __slots__ = ("rules", "counter", "distinct", "filled", "len_sum", "len_min", "len_max", "empties", "sample")

    def __init__(self, rules):
        self.rules = rules
        self.counter = Counter()
        self.distinct = None
        self.filled = self.len_sum = self.len_max = 0
        self.len_min = None
        self.empties = Counter()
        self.sample = []

    def feed(self, values):
        if self.counter is not None:
            self.counter.update(values)
        else:
            self._absorb(values, None)

    def _absorb(self, values, counts):
        """Учитывает значения в итогах; counts — сколько раз встретилось каждое (None — по одному)."""
        flags = self.rules.empty_flags(values)
        if True in flags:
            filled = list(compress(values, map(not_, flags)))
            if counts is None:
                self.empties.update(compress(values, flags))
            else:
                for value, times in zip(compress(values, flags), compress(counts, flags)):
                    self.empties[value] += times
                counts = list(compress(counts, map(not_, flags)))
            if len(self.empties) > 4096:              # мусора слишком много видов — храним главные
                self.empties = Counter(dict(self.empties.most_common(512)))
        else:
            filled = values
        if not filled:
            return
        lengths = list(map(len, filled))
        if counts is None:
            self.filled += len(filled)
            self.len_sum += sum(lengths)
        else:
            self.filled += sum(counts)
            self.len_sum += sum(map(mul, lengths, counts))
        low = min(lengths)
        self.len_min = low if self.len_min is None else min(self.len_min, low)
        self.len_max = max(self.len_max, max(lengths))
        if not self.sample:
            self.sample = list(filled[:200])
        self.distinct.add(self.rules.keys(filled))

    def cost(self):
        if self.counter is None:
            return self.distinct.cost()
        probe = list(islice(self.counter, 32))
        return len(self.counter) * (110 + sum(map(len, probe)) // max(len(probe), 1))

    def shrink(self):
        if self.counter is None:
            return self.distinct.shrink()
        counter, self.counter = self.counter, None
        self.distinct = Distinct()
        self._absorb(list(counter), list(counter.values()))
        return True

    def result(self, index, name):
        col, rules = Column(index, name), self.rules
        if self.counter is not None:                  # счётчик дожил до конца: всё точно
            values, counts = list(self.counter), list(self.counter.values())
            flags = rules.empty_flags(values)
            keep = list(map(not_, flags))
            filled, times = list(compress(values, keep)), list(compress(counts, keep))
            empties = Counter(dict(zip(compress(values, flags), compress(counts, flags))))
            merged, label = Counter(), {}
            for value, k, n in zip(filled, rules.keys(filled), times):
                merged[k] += n
                label.setdefault(k, value)
            col.filled, col.unique = sum(times), len(merged)
            col.top = [(label[k], n) for k, n in merged.most_common(TOP_KEEP)]
            lengths = list(map(len, filled))
            len_sum, sample = sum(map(mul, lengths, times)), filled[:200]
            if lengths:
                col.len_min, col.len_max = min(lengths), max(lengths)
        else:
            col.filled, col.exact = self.filled, self.distinct.exact
            col.unique = min(self.distinct.count(), self.filled)
            col.len_min, col.len_max = self.len_min or 0, self.len_max
            len_sum, sample, empties = self.len_sum, self.sample, self.empties
        col.len_avg = len_sum / col.filled if col.filled else 0.0
        col.empties = empties.most_common(8)
        col.examples = sample[:5]
        col.kind = guess_kind(sample)
        return col


class _RowTally:
    """Накопитель «сколько разных строк» — с тем же интерфейсом, что у столбцов."""

    counter = None

    def __init__(self):
        self.distinct = Distinct()
        self.cost, self.shrink = self.distinct.cost, self.distinct.shrink


def memory_budget():
    """Сколько байт можно занять счётчиками разных значений. Четверть лимита — запас:
    при переходе с яруса на ярус старая и новая структуры недолго живут вместе."""
    return (MEMORY_LIMIT_MB << 20) * 3 // 4


def _fit_memory(tallies, budget):
    """Держит накопители в отведённой памяти: самый тяжёлый переводится на ярус компактнее."""
    for tally in tallies:
        if tally.counter is not None and len(tally.counter) > COUNTER_LIMIT:
            tally.shrink()
    while True:
        loads = sorted(((tally.cost(), n) for n, tally in enumerate(tallies)), reverse=True)
        if sum(load for load, _ in loads) <= budget:
            return
        if not any(tallies[n].shrink() for _, n in loads):
            return


class Analysis:
    """Результат анализа файла."""

    def __init__(self):
        self.columns = []
        self.rows = 0                     # годных строк
        self.short = self.long = self.unreadable = self.repaired = 0
        self.unique_rows = 0
        self.rows_exact = True
        self.multiline = 0
        self.swallowed = 0                # строк, «проглоченных» значениями из-за непарных кавычек
        self.seconds = 0.0
        self.part = 1.0                   # доля прочитанного файла; меньше 1 — анализ прерван
        self.samples = []
        self.signature = None

    @property
    def broken(self):
        return self.short + self.long + self.unreadable

    def as_dict(self):
        return dict(rows=self.rows, broken=self.broken, short=self.short, long=self.long,
                    unreadable=self.unreadable, repaired=self.repaired,
                    duplicate_rows=max(0, self.rows - self.unique_rows), duplicate_rows_exact=self.rows_exact,
                    swallowed_lines=self.swallowed,
                    seconds=round(self.seconds, 2), complete=self.part >= 1,
                    columns=[col.as_dict(self.rows) for col in self.columns])


def analyze(src, rules, label="анализ"):
    """Один проход: заполненность и уникальность каждого столбца, дубли строк, битые строки."""
    tallies = [_Tally(rules) for _ in range(src.ncols)]
    whole = _RowTally()
    everything = [whole] + tallies        # при нехватке памяти точность «строк целиком» теряется последней
    budget = memory_budget()
    result = Analysis()
    with Scan(src, rules, label) as scan:
        try:
            for turn, block in enumerate(scan.blocks()):
                good, _ = scan.shape(block)
                if not good:
                    continue
                result.rows += len(good)
                for tally, values in zip(tallies, zip(*good)):
                    tally.feed(values)
                whole.distinct.add(rules.row_keys(good))
                if not turn % 8:
                    _fit_memory(everything, budget)
        except KeyboardInterrupt:
            scan.interrupted = True
        if scan.interrupted:
            result.part = min(scan.part, 0.999)
        result.columns = [tally.result(i, src.names[i]) for i, tally in enumerate(tallies)]
        result.unique_rows = min(whole.distinct.count(), result.rows)
        result.rows_exact = whole.distinct.exact
        result.short, result.long, result.unreadable = scan.short, scan.long, scan.unreadable
        result.repaired, result.samples = scan.repaired, scan.samples
        result.multiline = scan.multiline if result.part >= 1 else 0
        result.swallowed = scan.swallowed
        result.seconds = scan.seconds
    result.signature = (src.signature(), rules.signature())
    return result


class KeyCheck:
    """Результат проверки столбца или связки столбцов как ключа."""

    def __init__(self, indices):
        self.indices = indices
        self.filled = self.unique = 0
        self.exact = True
        self.top = []                     # [(значение, сколько)] — только повторяющиеся
        self.top_exact = True
        self.part = 1.0


def check_key(src, rules, indices, label="проверка"):
    """Отдельный проход по одному столбцу или связке: точное число разных значений и самые частые."""
    result = KeyCheck(indices)
    single = len(indices) == 1
    pick = itemgetter(*indices)
    counter, distinct, floor = Counter(), Distinct(), 0
    budget = memory_budget()
    with Scan(src, rules, label) as scan:
        try:
            for block in scan.blocks():
                good, _ = scan.shape(block)
                if single:
                    values = list(map(pick, good))
                    flags = rules.empty_flags(values)
                    if True in flags:
                        values = list(compress(values, map(not_, flags)))
                    keys = list(rules.keys(values))
                else:                                 # связка в счёт, только если заполнены все её части
                    parts = [p for p in map(pick, good) if all(map(rules.filled, p))]
                    keys = list(map(SEP.join, map(rules.keys, parts) if rules.soft else parts))
                result.filled += len(keys)
                counter.update(keys)
                distinct.add(keys)
                if distinct.cost() > budget:
                    distinct.shrink()
                while len(counter) > PRUNE_AT:        # слишком много разных — редкие забываем
                    floor += 1
                    counter = Counter({k: n for k, n in counter.items() if n > floor})
        except KeyboardInterrupt:
            scan.interrupted = True
        if scan.interrupted:
            result.part = min(scan.part, 0.999)
    result.unique, result.exact = min(distinct.count(), result.filled), distinct.exact
    result.top = [(k.replace(SEP, " · "), n) for k, n in counter.most_common(TOP_KEEP) if n > 1]
    result.top_exact = floor == 0
    return result


# ══════════════════════════════════════════════════════════════════════════════
#  ЭКСПОРТ: новый файл по плану
# ══════════════════════════════════════════════════════════════════════════════

def free_path(path):
    """Свободное имя: base_clean.csv, base_clean_2.csv …"""
    candidate, n = path, 1
    while candidate.exists():
        n += 1
        candidate = path.with_name(f"{path.stem}_{n}{path.suffix}")
    return candidate


class Plan:
    """Что сделать при экспорте. Номера столбцов — как в исходном файле, с нуля."""

    def __init__(self, src):
        self.keep = list(range(src.ncols))    # столбцы результата, по порядку
        self.dedupe = ""                      # "" — не трогать, "rows" — одинаковые строки, "key" — по ключу
        self.key = []                         # столбцы ключа
        self.min_filled = 0                   # убрать строки, где заполнено меньше стольких полей
        self.require = []                     # столбцы, которые должны быть заполнены
        self.require_any = False              # … достаточно любого одного
        self.broken = "file"                  # битые строки: "file" — в отдельный файл, "skip" — выбросить
        self.trim = self.denoise = self.blank = False
        self.encoding = src.encoding
        self.delimiter = src.delimiter
        self.header = src.has_header
        self.quote_all = False
        self.path = free_path(src.path.with_name(src.path.stem + OUTPUT_SUFFIX + (src.path.suffix or ".csv")))


class Outcome:
    """Итог экспорта."""

    def __init__(self):
        self.rows = self.written = self.duplicates = self.sparse = self.lacking = 0
        self.short = self.long = self.unreadable = self.repaired = 0
        self.path = self.broken_path = None
        self.size = 0
        self.seconds = 0.0
        self.cancelled = False

    @property
    def broken(self):
        return self.short + self.long + self.unreadable


class Sink:
    """Файл результата. Пишется во временный *.part и получает своё имя только при успехе."""

    def __init__(self, path, encoding, delimiter, quote, quote_all, lineterm):
        self.path = Path(path)
        self.temp = self.path.with_name(self.path.name + ".part")
        self.temp.parent.mkdir(parents=True, exist_ok=True)
        self.file = open(self.temp, "w", encoding=encoding, errors="replace", newline="", buffering=1 << 20)
        if quote:
            self.write = csv.writer(self.file, delimiter=delimiter, quotechar=quote, lineterminator=lineterm,
                                    quoting=csv.QUOTE_ALL if quote_all else csv.QUOTE_MINIMAL).writerows
        else:                             # файл без кавычек: поля просто склеиваются разделителем
            put, glue = self.file.write, delimiter.join
            self.write = lambda rows: put("".join([glue(row) + lineterm for row in rows]))

    def commit(self):
        self.file.close()
        os.replace(self.temp, self.path)

    def discard(self):
        """Убирает временный файл, если он ещё есть (после commit его уже нет)."""
        self.file.close()
        try:
            self.temp.unlink()
        except OSError:
            pass


# Чистка значений: что удалить совсем и по чему видно, что значение нуждается в чистке.
_GONE = dict.fromkeys(itertools.chain(range(0x00, 0x09), range(0x0e, 0x1c), range(0x7f, 0x85), range(0x86, 0xa0),
                                      (0xad,), range(0x200b, 0x2010), range(0x202a, 0x202f), range(0x2060, 0x206a),
                                      (0xfeff,)))
_ODD = re.compile("[\x00-\x1f\x7f-\xa0\xad\u1680\u2000-\u200f\u2028-\u202f\u205f-\u2069\u3000\ufeff]").search
_EDGE = "\ue000"


def _tidy(value):
    """Невидимые символы — прочь; переносы, табуляции и повторные пробелы — в один пробел."""
    return " ".join(value.translate(_GONE).split())


def _tidy_rows(rows):
    out = []
    for row in rows:
        joined = _EDGE.join(row)              # одна проверка на строку: чистых строк большинство
        if (_ODD(joined) or "  " in joined or _EDGE + " " in joined or " " + _EDGE in joined
                or joined[:1] == " " or joined[-1:] == " "):
            row = list(map(_tidy, row))
        out.append(row)
    return out


def export(src, rules, plan, label="экспорт"):
    """Один проход: читает исходный файл, пишет новый по плану. Исходный файл не трогает."""
    out_path = Path(plan.path)
    if out_path.exists() and os.path.samefile(out_path, src.path):
        raise ValueError("результат нельзя писать поверх исходного файла")
    if not plan.keep:
        raise ValueError("в результате должен остаться хотя бы один столбец")
    broken_path = out_path.with_name(f"{out_path.stem}_broken{out_path.suffix}")
    if broken_path.exists() and os.path.samefile(broken_path, src.path):
        broken_path = free_path(broken_path)

    # Рабочая строка: сначала столбцы результата, за ними — нужные только для проверок.
    work = list(plan.keep)

    def position(column):
        if column not in work:
            work.append(column)
        return work.index(column)

    need = [position(c) for c in plan.require]
    key_at = [position(c) for c in plan.key] if plan.dedupe == "key" else []
    shown = len(plan.keep)
    extra = len(work) > shown
    if work == list(range(src.ncols)):
        project = None
    else:
        project = itemgetter(*work) if len(work) > 1 else itemgetter(slice(work[0], work[0] + 1))

    filled, no_data, nulls, soft = rules.filled, rules.no_data, rules.nulls, rules.soft
    minimum = min(plan.min_filled, shown)
    seen = HashSet64() if plan.dedupe else None

    def key_hash(row):
        parts = [row[i] for i in key_at]
        if not all(map(filled, parts)):
            return 0                              # ключ неполон — строка не дубль, как NULL в SQL
        if soft:
            parts = [part.strip().casefold() for part in parts]
        return hash(SEP.join(parts)) & MASK64 or 1

    # Кавычки в результате — как в исходном; если их там нет, а поле может содержать разделитель — ставим.
    quote = src.quote or ('"' if plan.delimiter != src.delimiter or plan.quote_all or src.escape else "")
    result = Outcome()
    result.path = out_path
    sink = Sink(out_path, plan.encoding, plan.delimiter, quote, plan.quote_all, src.lineterm)
    side = None                                   # файл для битых строк — создаётся, только если они есть
    try:
        with Scan(src, rules, label) as scan:
            if plan.header and src.has_header:
                sink.write([[src.names[c] for c in plan.keep]])
            try:
                for block in scan.blocks():
                    rows, bad = scan.shape(block)
                    if bad and plan.broken == "file":
                        if side is None:
                            side = Sink(broken_path, plan.encoding, plan.delimiter, quote, plan.quote_all, src.lineterm)
                        side.write(bad)
                    if not rows:
                        continue
                    result.rows += len(rows)
                    if project:
                        rows = list(map(project, rows))
                    if plan.denoise:
                        rows = _tidy_rows(rows)
                    elif plan.trim:
                        rows = [list(map(str.strip, row)) for row in rows]
                    if plan.blank:
                        rows = [["" if no_data(v) or v.strip() in nulls else v for v in row] for row in rows]
                    if minimum:
                        before = len(rows)
                        if minimum == 1 and not extra:
                            rows = [row for row in rows if any(map(filled, row))]
                        else:
                            rows = [row for row in rows if rules.empty_flags(row[:shown]).count(False) >= minimum]
                        result.sparse += before - len(rows)
                    if need:
                        before, test = len(rows), any if plan.require_any else all
                        rows = [row for row in rows if test(map(filled, map(row.__getitem__, need)))]
                        result.lacking += before - len(rows)
                    if seen is not None:
                        before = len(rows)
                        if plan.dedupe == "key":
                            hashes = list(map(key_hash, rows))
                        else:
                            keys = rules.row_keys([row[:shown] for row in rows] if extra else rows)
                            hashes = map(MASK64.__and__, map(hash, keys))
                        rows = list(compress(rows, seen.add_flags(hashes)))
                        result.duplicates += before - len(rows)
                    if extra:
                        rows = [row[:shown] for row in rows]
                    sink.write(rows)
                    result.written += len(rows)
            except KeyboardInterrupt:
                scan.interrupted = True
            result.cancelled = scan.interrupted
            result.short, result.long, result.unreadable = scan.short, scan.long, scan.unreadable
            result.repaired, result.seconds = scan.repaired, scan.seconds
        if not result.cancelled:
            sink.commit()
            if side:
                side.commit()
                result.broken_path = broken_path
            result.size = out_path.stat().st_size
    finally:                                      # что не доведено до конца — не оставляет следов
        sink.discard()
        if side:
            side.discard()
    return result


# ══════════════════════════════════════════════════════════════════════════════
#  ВЫБОР СТОЛБЦОВ ТЕКСТОМ: «1 3-5 email phone*»
# ══════════════════════════════════════════════════════════════════════════════

def well_filled(an, index, percent):
    """Столбец не пуст и заполнен не меньше чем на percent процентов."""
    filled = an.columns[index].filled
    return filled > 0 and 100 * filled >= percent * an.rows


def parse_columns(text, names):
    """Номера (с единицы), диапазоны, имена и маски имён → номера столбцов с нуля, без повторов."""
    text = re.sub(r"(?<=\d)\s*[-–—]\s*(?=\d)", "-", text)
    folded = [name.strip().casefold() for name in names]
    found = []
    for quoted, bare in re.findall(r'"([^"]*)"|([^\s,;]+)', text):
        token = quoted or bare
        match = None if quoted else re.fullmatch(r"(\d+)(?:-(\d+))?", token)
        picked = []
        if match:
            first, last = int(match.group(1)), int(match.group(2) or match.group(1))
            if 1 <= first <= len(names) and 1 <= last <= len(names):
                step = 1 if last >= first else -1
                picked = list(range(first - 1, last - 1 + step, step))
        if not picked:
            wanted = token.strip().casefold()
            picked = [i for i, name in enumerate(folded) if name and name == wanted]
            if not picked and ("*" in wanted or "?" in wanted):
                mask = re.compile(re.escape(wanted).replace(r"\*", ".*").replace(r"\?", "."))
                picked = [i for i, name in enumerate(folded) if name and mask.fullmatch(name)]
        if not picked:
            raise ValueError(f"нет столбца «{token}» — всего столбцов {len(names)}")
        found.extend(picked)
    return list(dict.fromkeys(found))


# ══════════════════════════════════════════════════════════════════════════════
#  ВЫВОД РЕЗУЛЬТАТОВ
# ══════════════════════════════════════════════════════════════════════════════

def file_card(src):
    print(f"  {bold(clip_left(show(src.path.name), term_width() - 16))}   {dim(size_text(src.size))}")
    parts = [encoding_name(src.encoding), f"разделитель {delimiter_name(src.delimiter)}",
             f"{num(src.ncols)} {plural(src.ncols, 'столбец', 'столбца', 'столбцов')}",
             "с заголовком" if src.has_header else "без заголовка"]
    if not src.quote:
        parts.insert(2, "без кавычек")
    note(" · ".join(parts))
    if src.note:
        alert(src.note)


def table_lines(an, order="n", width=None):
    """Таблица столбцов: заполненность, уникальность, тип, пример."""
    width = width or term_width()
    columns = list(an.columns)
    if order == "f":
        columns.sort(key=lambda c: -c.filled)
    elif order == "u":
        columns.sort(key=lambda c: -c.unique)
    num_w = len(str(len(columns)))
    name_w = min(max([wlen(c.name) for c in columns] + [7]), 26)
    count_w = max(len(num(an.rows)), 9)
    kind_w = max([wlen(c.kind) for c in columns] + [0])
    bar_w = 10

    def total():
        return (2 + num_w + 2 + name_w + 2 + count_w + 2 + 5 + (2 + bar_w if bar_w else 0)
                + 2 + count_w + 2 + 2 + 5 + (2 + kind_w if kind_w else 0))

    if total() > width:                   # узкое окно: сначала уходит тип, затем полоска, затем сжимается имя
        kind_w = 0
    if total() > width:
        bar_w = 0
    if total() > width:
        name_w = max(8, name_w - (total() - width))
    example_w = width - total() - 2
    if example_w < 10:
        example_w = 0

    def line(cells, paints):
        return "  " + "  ".join(paint(cell) for cell, paint in zip(cells, paints) if cell is not None).rstrip()

    lines = [line([pad("№", num_w, True), pad("столбец", name_w), pad("заполнено", count_w, True), pad("%", 5, True),
                   " " * bar_w if bar_w else None, pad("уникальных", count_w + 2, True), pad("%", 5, True),
                   pad("тип", kind_w) if kind_w else None, "пример" if example_w else None], [dim] * 9)]
    for c in columns:
        unique = num(c.unique) if c.exact else "≈ " + num(roughly(c.unique))
        cells = [pad(str(c.index + 1), num_w, True), pad(c.name or "—", name_w), pad(num(c.filled), count_w, True),
                 pad(pct(c.filled, an.rows), 5, True), meter(0, bar_w) if bar_w else None,
                 pad(unique, count_w + 2, True), pad(pct(c.unique, c.filled), 5, True),
                 pad(c.kind, kind_w) if kind_w else None,
                 clip(show(c.examples[0]) if c.examples else "", example_w) if example_w else None]
        if not c.filled:                  # пустой столбец — вся строка приглушена
            lines.append(line(cells, [dim, dim, dim, dim, str, dim, dim, dim, dim]))
            continue
        if bar_w:
            cells[4] = meter(c.filled / an.rows, bar_w)
        is_key = c.exact and c.unique == c.filled
        lines.append(line(cells, [dim, str, str, str, str, str, accent if is_key else str, dim, dim]))
    return lines


def summary_lines(an, width=None):
    """Итог под таблицей: строки, битые, дубли, время."""
    width = width or term_width()
    rows = [("строк", num(an.rows), "")]
    total = an.rows + an.broken
    if an.broken:
        detail = [f"{name} {num(n)}" for name, n in (("коротких", an.short), ("длинных", an.long),
                                                     ("нечитаемых", an.unreadable)) if n]
        rows.append(("битых", num(an.broken), f"{pct(an.broken, total)} %   " + " · ".join(detail)))
    if an.repaired:
        rows.append(("починено", num(an.repaired), "дополнены или обрезаны до нужного числа полей"))
    duplicates = max(0, an.rows - an.unique_rows)
    if an.rows_exact:
        rows.append(("дублей строк", num(duplicates), f"{pct(duplicates, an.rows)} %" if duplicates else ""))
    else:
        rows.append(("дублей строк", repeats_text(an.rows, an.unique_rows, False),
                     "оценка: не хватило памяти (MEMORY_LIMIT_MB)"))
    if an.multiline:
        rows.append(("переносов", num(an.multiline), "переводы строки внутри значений"))
    speed = f"{short(int(total / an.seconds))} строк/с" if an.seconds > 0.5 else ""
    rows.append(("время", time_text(an.seconds), speed))
    key_w = max(wlen(k) for k, _, _ in rows)
    value_w = max(wlen(v) for _, v, _ in rows)
    lines = [f"  {dim(pad(k, key_w))}  {pad(v, value_w, True)}   {dim(extra)}".rstrip() for k, v, extra in rows]
    warnings = []
    if an.part < 1:
        warnings.append(f"прервано на {an.part * 100:.0f} % файла — цифры по прочитанной части")
    if an.swallowed:
        lost = f"{num(an.swallowed)} {plural(an.swallowed, 'строка', 'строки', 'строк')}"
        warnings.append(f"похоже на непарные кавычки: внутри значений оказалось {lost} файла, "
                        "похожих на отдельные записи")
        warnings.append("если кавычки в этом файле — просто символы, выключи их: «Формат» → «Кавычки»")
    elif an.unreadable or an.broken * 100 > total:
        warnings.append("много битых строк — проверь разделитель и кавычки в разделе «Формат»")
    for text in warnings:
        lines += ["  " + yellow(line) for line in textwrap.wrap(text, width - 2)]
    return lines


def value_list(pairs, total, limit=10):
    """Строки «сколько · доля · значение»."""
    pairs = pairs[:limit]
    if not pairs:
        return []
    count_w = max(len(num(n)) for _, n in pairs)
    room = term_width() - count_w - 17
    return [f"    {pad(num(n), count_w, True)}  {dim(pad(pct(n, total) + ' %', 7, True))}  "
            + (clip(show(value), room) if value else dim("пустая строка")) for value, n in pairs]


def column_card(an, col):
    """Подробности по одному столбцу."""
    empty = an.rows - col.filled
    approx = "" if col.exact else "≈ "
    unique = col.unique if col.exact else roughly(col.unique)
    bar = meter(col.filled / an.rows if an.rows else 0)
    rows = [("заполнено", num(col.filled), pct(col.filled, an.rows) + " %", bar),
            ("пусто", num(empty), pct(empty, an.rows) + " %", ""),
            ("уникальных", approx + num(unique), pct(col.unique, col.filled) + " %", dim("от заполненных")),
            ("повторов", repeats_text(col.filled, col.unique, col.exact), "", "")]
    value_w = max(wlen(row[1]) for row in rows)
    for name, value, share, extra in rows:
        print(f"  {dim(pad(name, 10))}  {pad(value, value_w, True)}  {pad(share, 7, True)}  {extra}".rstrip())
    if col.filled:
        span = f"{col.len_min} {plural(col.len_min, 'символ', 'символа', 'символов')}"
        if col.len_min != col.len_max:
            span = f"от {col.len_min} до {col.len_max}, в среднем {col.len_avg:.1f}"
        print(f"  {dim(pad('длина', 10))}  {span}")
    if not col.exact:
        note("число уникальных — оценка: на точный счёт по всем столбцам сразу не хватило памяти")
    if col.empties and empty:
        print()
        note("что сочтено пустым")
        for line in value_list(col.empties, an.rows, 5):
            print(line)
    print()
    if col.top is None:
        note("примеры")
        for value in col.examples:
            print("    " + clip(show(value), term_width() - 6))
    elif col.top and col.top[0][1] > 1:
        note("частые значения" if col.top_exact else "частые значения (счёт приблизительный)")
        for line in value_list([pair for pair in col.top if pair[1] > 1], col.filled):
            print(line)
    elif col.filled:
        note("повторяющихся значений не найдено")


def key_card(check):
    """Итог проверки столбца или связки столбцов как ключа."""
    approx = "" if check.exact else "≈ "
    unique = check.unique if check.exact else roughly(check.unique)
    complete = dim("все части ключа заполнены") if len(check.indices) > 1 else ""
    rows = [("строк с ключом", num(check.filled), complete),
            ("уникальных", approx + num(unique), pct(check.unique, check.filled) + " %"),
            ("повторов", repeats_text(check.filled, check.unique, check.exact), "")]
    value_w = max(wlen(row[1]) for row in rows)
    for name, value, extra in rows:
        print(f"  {dim(pad(name, 14))}  {pad(value, value_w, True)}  {extra}".rstrip())
    if check.part < 1:
        alert(f"прервано на {check.part * 100:.0f} % файла — цифры по прочитанной части")
    print()
    if check.top:
        note("самые частые" if check.top_exact else "самые частые (счёт приблизительный)")
        for line in value_list(check.top, check.filled):
            print(line)
    elif check.exact and check.filled == check.unique > 0 and check.part >= 1:
        print("  " + green("повторов нет — годится как ключ"))


def broken_card(samples):
    """Примеры битых строк: номер записи, сколько в ней полей и сами поля."""
    for number, row in samples:
        print(f"  {dim('запись')} {num(number)}   {yellow(f'полей: {len(row)}')}")
        print("    " + clip(" ¦ ".join(show(cell) for cell in row), term_width() - 6))


def outcome_lines(result, plan):
    rows = [("прочитано", result.rows, ""), ("записано", result.written, "")]
    if plan.dedupe:
        rows.append(("дублей убрано", result.duplicates, ""))
    if plan.min_filled:
        rows.append(("пустых убрано", result.sparse, ""))
    if plan.require:
        rows.append(("без обязательных", result.lacking, ""))
    if result.broken:
        where = []
        if result.short + result.long:
            where.append(f"→ {result.broken_path.name}" if result.broken_path else "выброшены")
        if result.unreadable:
            where.append(f"нечитаемых {num(result.unreadable)} — пропущены")
        rows.append(("битых", result.broken, " · ".join(where)))
    if result.repaired:
        rows.append(("починено", result.repaired, ""))
    key_w = max(wlen(k) for k, _, _ in rows)
    value_w = max(len(num(v)) for _, v, _ in rows)
    room = max(term_width() - key_w - value_w - 7, 10)
    return [f"  {dim(pad(k, key_w))}  {pad(num(v), value_w, True)}   {dim(clip(extra, room))}".rstrip()
            for k, v, extra in rows]


def report_text(src, an, width=160):
    """Отчёт анализа простым текстом."""
    was, UI.color = UI.color, False
    try:
        head = [str(src.path), f"{size_text(src.size)} · {encoding_name(src.encoding)} · разделитель "
                               f"{delimiter_name(src.delimiter)} · столбцов: {src.ncols} · "
                               f"{'с заголовком' if src.has_header else 'без заголовка'}", ""]
        body = table_lines(an, width=width) + [""] + summary_lines(an, width)
        return "\n".join(head + [line[2:] for line in body]) + "\n"
    finally:
        UI.color = was


def save_report(src, an, path):
    """Отчёт в файл: .json — всё подробно, .csv — таблица столбцов, иначе — текст."""
    path = Path(path)
    kind = path.suffix.lower()
    if kind == ".json":
        data = dict(file=str(src.path), size=src.size, encoding=src.encoding, delimiter=src.delimiter,
                    header=src.has_header, **an.as_dict())
        path.write_text(json.dumps(data, ensure_ascii=False, indent=2), encoding="utf-8")
    elif kind == ".csv":
        with open(path, "w", encoding="utf-8-sig", newline="") as file:
            writer = csv.writer(file, delimiter=";")
            writer.writerow(["№", "столбец", "заполнено", "заполнено %", "уникальных", "уникальных %", "точно",
                             "тип", "длина мин", "длина макс", "длина средняя", "пример"])
            for c in an.columns:
                d = c.as_dict(an.rows)
                writer.writerow([d["number"], c.name, c.filled, d["filled_percent"], c.unique, d["unique_percent"],
                                 "да" if c.exact else "нет", c.kind, c.len_min, c.len_max, d["length_avg"],
                                 c.examples[0] if c.examples else ""])
    else:
        path.write_text(report_text(src, an), encoding="utf-8")
    return path


# ══════════════════════════════════════════════════════════════════════════════
#  ИНТЕРАКТИВНЫЙ РЕЖИМ
# ══════════════════════════════════════════════════════════════════════════════

def clean_path(text):
    """Путь, как его вставляют и перетаскивают: в кавычках, с «& » от PowerShell, ссылкой file://, с ~."""
    text = text.strip()
    if text.startswith("& "):
        text = text[2:].strip()
    if len(text) > 1 and text[0] == text[-1] and text[0] in "\"'":
        text = text[1:-1]
    if text.lower().startswith("file://"):
        text = unquote(text[7:])
        if os.name == "nt":               # file:///C:/base.csv и file://server/share/base.csv
            text = text.lstrip("/") if re.match(r"/+[A-Za-z]:", text) else "//" + text.lstrip("/")
    return Path(os.path.expandvars(os.path.expanduser(text)))


def dropped(text):
    """Что перетащили в окно или вписали руками: существующие файлы и папки из строки ввода.

    Терминалы вставляют путь по-разному: в кавычках, если в нём пробелы, или с экранированными
    пробелами; несколько файлов — через пробел. Сначала вся строка пробуется как один путь.
    """
    def existing(path):
        try:
            return Path(os.path.abspath(path)) if path.exists() else None
        except (OSError, ValueError):
            return None

    if not text.strip():
        return []
    whole = existing(clean_path(text))
    if whole:
        return [whole]
    found = []
    bare = r"(\S+)" if os.name == "nt" else r"((?:\\.|\S)+)"
    for double, single, plain in re.findall(r'"([^"]+)"|\'([^\']+)\'|' + bare, text.strip()):
        if plain and os.name != "nt":
            plain = re.sub(r"\\(.)", r"\1", plain)        # my\ base.csv → my base.csv
        path = existing(clean_path(double or single or plain))
        if path and path not in found:
            found.append(path)
    return found


def recent_files():
    """Недавно открытые базы, последняя — первой. Список живёт в домашней папке."""
    if RECENT_FILES <= 0:
        return []
    try:
        lines = (Path.home() / RECENT_NAME).read_text(encoding="utf-8").splitlines()
    except (OSError, RuntimeError, UnicodeError):
        return []
    return [Path(line) for line in lines if line.strip()][:RECENT_FILES]


def remember(path):
    if RECENT_FILES <= 0:
        return
    path = Path(os.path.abspath(path))
    paths = [path] + [p for p in recent_files() if p != path]
    try:
        (Path.home() / RECENT_NAME).write_text("".join(f"{p}\n" for p in paths[:RECENT_FILES]), encoding="utf-8")
    except (OSError, RuntimeError, UnicodeError):
        pass                              # не записалось — значит, без списка недавних


def data_files(folder):
    """Файлы в папке, похожие на базы; свежие — первыми."""
    try:
        files = [p for p in Path(folder).iterdir()
                 if p.suffix.lower() in DATA_SUFFIXES and not p.name.endswith("_report.txt")
                 and p.is_file() and p.stat().st_size]
        return sorted(files, key=lambda p: -p.stat().st_mtime)[:9]
    except OSError:
        return []


def size_of(path):
    try:
        return size_text(path.stat().st_size)
    except OSError:
        return ""


BANNER = "анализ и очистка текстовых баз"


class App:
    """Интерактивный режим. Каждый раздел рисуется на чистом экране, а то, что нужно сказать
    по итогам действия (ошибка, «сохранено»), показывается внизу следующего экрана."""

    def __init__(self, rules, forced=None):
        self.rules = rules
        self.forced = dict(forced or {})  # параметры формата, заданные вручную
        self.src = self.analysis = self.plan = None
        self.notes = []                   # сообщения для следующего экрана: (краска, текст)

    # ── экран ─────────────────────────────────────────────────────────────────

    def page(self, screen="", extra=""):
        """Новый экран: чистый лист и строка «файл › раздел»."""
        clear_screen()
        print()
        if screen:
            room = term_width() - 2
            screen = clip(screen, max(room - 16, 20))
            name = clip_left(show(self.src.path.name), max(room - wlen(screen) - 3, 8))
            left = room - wlen(name) - wlen(screen) - 6
            extra = clip(extra, left) if extra and left > 8 else ""
            print(f"  {dim(name + ' ›')} {bold(screen)}" + (f"   {dim(extra)}" if extra else ""))
            print()

    def tell(self, text, paint=dim):
        """Сообщение, которое появится внизу следующего экрана."""
        self.notes.append((paint, text))

    def foot(self, *pairs):
        """Низ экрана: накопившиеся сообщения и подсказки по клавишам."""
        if self.notes:
            print()
            for paint, text in self.notes:
                say(text, paint)
            self.notes.clear()
        if pairs:
            print()
            hints(*pairs)

    # ── файл ──────────────────────────────────────────────────────────────────

    def open(self, path):
        forced = self.forced if self.src is None else {}      # формат из командной строки — для первого файла
        src = Source(path, forced)
        if not src.ncols:
            raise ValueError("файл пуст или в нём нет строк с данными")
        self.src, self.forced, self.analysis, self.plan = src, src.forced, None, Plan(src)
        remember(src.path)

    def choose_file(self, typed=""):
        """Экран выбора базы: перетащить файл в окно, взять из недавних или из папки.

        typed — уже готовый ввод: путь из командной строки или файл, перетащенный прямо в меню.
        Возвращает True, если файл открыт.
        """
        folder, offered = None, []        # folder — папка, которую перетащили; None — текущая
        while True:
            if typed:
                found, text, typed = dropped(typed), typed, ""
            else:
                options = self.file_list(folder, offered)
                pairs = [("номер", "открыть из списка")] if options else [("путь", "можно вписать и вручную")]
                self.foot(*pairs, ("0", "назад" if self.src else "выход"))
                text = ask(drop=False)
                if text == "0" or key(text) == "q" or not text and self.src:
                    return False
                if not text:
                    continue
                if text.isdigit() and 1 <= int(text) <= len(options):
                    found = [options[int(text) - 1]]
                else:
                    found = dropped(text)
            files = [p for p in found if p.is_file()]
            if len(found) == 1 and files:
                try:
                    self.open(files[0])
                    return True
                except (OSError, ValueError) as error:
                    self.tell(f"не открыть {show(files[0].name)} — {reason(error)}", red)
            elif files:                   # перетащили несколько — работаем с одним, какой?
                offered = files
            elif found:                   # перетащили папку — показываем, что в ней
                folder = found[0]
                if not data_files(folder):
                    self.tell(f"в папке {show(folder.name)} нет файлов, похожих на базы "
                              f"({', '.join(DATA_SUFFIXES)})", yellow)
            else:
                self.tell(f"нет такого файла: {show(text)}", red)

    def file_list(self, folder, offered):
        """Рисует экран выбора базы и возвращает файлы в том порядке, в каком они пронумерованы."""
        current = Path(os.path.abspath(self.src.path)) if self.src else None
        here = folder or Path.cwd()
        nearby = (f"в папке {show(here.name or str(here))}", data_files(here))
        recent = ("недавние", [p for p in recent_files() if p.is_file()])
        groups = (("перетащено", offered), nearby, recent) if folder else (("перетащено", offered), recent, nearby)
        clear_screen()
        print()
        print(f"  {bold('tdb')}  {dim(BANNER)}")
        print()
        print(f"  Перетащи файл базы в это окно и нажми {bold('Enter')}")
        options, shown = [], []
        for label, paths in groups:                       # один файл — в одной группе, первой по порядку
            paths = [p for p in paths if p != current and not any(p in group for _, group in shown)][:9]
            if paths:
                shown.append((label, paths))
        width = min(max([wlen(show(p.name)) for _, group in shown for p in group] + [8]), 48, term_width() - 20)
        for label, paths in shown:
            print()
            note(label)
            for path in paths:
                options.append(path)
                room = term_width() - width - 21                  # место под папку, если файл не отсюда
                where = clip_left(show(str(path.parent)), room) if path.parent != here and room > 12 else ""
                print(f"  {accent(pad(str(len(options)), 2, True))}  {pad(show(path.name), width)}   "
                      f"{dim(pad(size_of(path), 9, True))}   {dim(where)}".rstrip())
        return options

    def analyzed(self):
        """Анализ файла; считается один раз и пересчитывается, если изменились файл или правила."""
        signature = (self.src.signature(), self.rules.signature())
        if self.analysis is None or self.analysis.signature != signature:
            self.analysis = analyze(self.src, self.rules)
        return self.analysis

    # ── главное меню ──────────────────────────────────────────────────────────

    def run(self, path=""):
        UI.interactive, UI.steps = True, not UI.live
        try:
            opened = self.choose_file(path)
        except Back:
            opened = False
        while opened:
            an = self.analysis
            if an and an.signature != (self.src.signature(), self.rules.signature()):
                an = self.analysis = None
            self.page()
            print(f"  {bold('tdb')}  {dim(BANNER)}")
            print()
            file_card(self.src)
            print()
            ready = ""
            if an:
                ready = "готов" if an.part >= 1 else f"прерван на {an.part * 100:.0f} %"
                ready += f" · {num(an.rows)} {plural(an.rows, 'строка', 'строки', 'строк')}"
            menu([("1", "Анализ", ready or "заполненность и уникальность столбцов", bool(ready)),
                  ("2", "Просмотр", "записи файла по одной"),
                  ("3", "Экспорт", "новый файл: столбцы, дубли, пустые и битые строки"),
                  ("4", "Формат", "кодировка, разделитель, заголовок, что считать пустым"),
                  ("5", "Файл", "открыть другой — или просто перетащи его в это окно"),
                  ("0", "Выход", "")])
            self.foot()
            try:
                text = ask(drop=False)
            except Back:
                break
            choice = key(text)
            try:
                if choice == "1":
                    self.screen_analysis()
                elif choice == "2":
                    self.screen_preview()
                elif choice == "3":
                    self.screen_export()
                elif choice == "4":
                    self.screen_format()
                elif choice == "5":
                    self.choose_file()
                elif choice in ("0", "q"):
                    break
                elif dropped(text):       # в меню перетащили другой файл
                    self.choose_file(text)
                elif text:
                    self.tell(f"нет пункта «{show(text)}» — нужен номер из меню или путь к файлу")
            except Back:
                pass
            except Dropped as drop:       # на любом экране перетащили другую базу
                try:
                    self.choose_file(str(drop))
                except Back:
                    pass
            except (OSError, ValueError) as error:
                self.tell(reason(error), red)
        clear_screen()

    # ── анализ ────────────────────────────────────────────────────────────────

    def screen_analysis(self):
        src = self.src
        if self.analysis is None:
            self.page("Анализ")
        an, order, view = self.analyzed(), "n", None      # view: None — таблица, "broken", Column или KeyCheck
        while True:
            if view is None:
                self.page("Анализ")
                lines = table_lines(an, order)
                if len(lines) + 14 > term_height():       # таблица выше окна — шапка повторяется внизу
                    lines.append(lines[0])
                for line in lines + [""] + summary_lines(an):
                    print(line)
                pairs = [("номер", "столбец подробно"), ("несколько номеров", "проверить связку как ключ"),
                         ("f u n", "сортировка: заполненность, уникальность, порядок"), ("s", "сохранить отчёт")]
                if an.samples:
                    pairs.append(("b", "примеры битых строк"))
                if an.part < 1:
                    pairs.append(("r", "посчитать заново"))
                back = "в меню"
            elif view == "broken":
                self.page("Анализ › Битые строки", f"в строке должно быть полей: {src.ncols}")
                broken_card(an.samples)
                pairs, back = [("номер", "столбец подробно")], "к таблице"
            elif isinstance(view, Column):
                self.page(f"Анализ › {view.index + 1} · {show(view.name) or 'без имени'}", view.kind)
                column_card(an, view)
                pairs, back = [("номер", "другой столбец")], "к таблице"
                if view.top is None:
                    what = "частые значения" if view.exact else "точное число уникальных и частые значения"
                    pairs.append(("d", f"{what}: отдельный проход"))
            else:
                self.page("Анализ › Ключ: " + src.labels(view.indices))
                key_card(view)
                pairs, back = [("номера", "другой столбец или связка")], "к таблице"
            self.foot(*pairs, ("Enter", back))
            try:
                text = ask()
            except Back:
                text = ""
            command = key(text)
            if not text:
                if view is None:
                    return
                view = None
            elif command in ("f", "u", "n"):
                order, view = command, None
            elif command == "s":
                path = save_report(src, an, free_path(src.path.with_name(src.path.stem + "_report.txt")))
                self.tell(f"отчёт сохранён → {show(path.name)} — рядом с базой", green)
            elif command == "b" and an.samples:
                view = "broken"
            elif command == "r":
                self.analysis = None
                self.page("Анализ")
                an, view = self.analyzed(), None
            elif command == "d" and isinstance(view, Column) and view.top is None:
                self.page(f"Анализ › {view.index + 1} · {show(view.name) or 'без имени'}", view.kind)
                self.deepen(view)
            else:
                try:
                    picked = parse_columns(text, src.names)
                except ValueError as error:
                    self.tell(str(error), red)
                    continue
                if len(picked) == 1:
                    view = an.columns[picked[0]]
                else:
                    self.page("Анализ › Ключ: " + src.labels(picked))
                    view = check_key(src, self.rules, picked)

    def deepen(self, col):
        """Отдельный проход по столбцу: частые значения и точное число уникальных."""
        check = check_key(self.src, self.rules, [col.index], "частые значения")
        if check.part < 1:
            self.tell("прервано — частые значения не посчитаны", yellow)
            return
        col.top, col.top_exact = check.top, check.top_exact
        if check.exact:
            col.unique, col.exact = check.unique, True

    # ── просмотр ──────────────────────────────────────────────────────────────

    def screen_preview(self):
        src = self.src
        stream, number, hide_empty, end = records(src), 0, True, object()

        def jump(target):
            """Встать перед записью target (нумерация с единицы)."""
            fresh = records(src)
            next(islice(fresh, target - 1, target - 1), None)
            return fresh, target - 1

        while True:
            row = next(stream, end)
            self.page("Просмотр")
            if row is end:
                note("конец файла — дальше снова с первой записи")
                stream, number = records(src), 0
            else:
                number += 1
                self.record_card(number, row, hide_empty)
            self.foot(("Enter", "следующая"), ("число", "перейти к записи"), ("e", "пустые поля: показать / скрыть"),
                      ("0", "в меню"))
            text = ask()
            command = key(text)
            if command in ("0", "q"):
                return
            if command == "e":
                hide_empty = not hide_empty
                stream, number = jump(max(number, 1))
            elif text.isdigit():
                stream, number = jump(int(text))
            elif text:
                stream, number = jump(max(number, 1))
                self.tell("Enter — следующая запись, число — перейти к записи с этим номером")

    def record_card(self, number, row, hide_empty):
        src = self.src
        if row is None:
            print(f"  {dim('запись')} {bold(num(number))}   {yellow('нечитаема: поле больше предела')}")
            return
        if not any(row):
            print(f"  {dim('запись')} {bold(num(number))}   {dim('пустая строка файла')}")
            return
        warning = "" if len(row) == src.ncols else "   " + yellow(f"полей {len(row)} вместо {src.ncols} — битая")
        print(f"  {dim('запись')} {bold(num(number))}{warning}")
        num_w = len(str(max(len(row), 1)))
        name_w = min(max([wlen(name) for name in src.names] + [1]), 24)
        hidden, room = 0, term_width() - num_w - name_w - 6
        for i, value in enumerate(row):
            if hide_empty and not self.rules.filled(value):
                hidden += 1
                continue
            name = src.names[i] if i < src.ncols else ""
            print(f"  {dim(pad(str(i + 1), num_w, True))}  {dim(pad(name or '—', name_w))}  {clip(show(value), room)}")
        if hidden:
            note(f"пустых полей скрыто: {hidden}")

    # ── экспорт ───────────────────────────────────────────────────────────────

    def plan_rows(self):
        src, plan = self.src, self.plan
        whole = plan.keep == list(range(src.ncols))
        columns = f"все {src.ncols}" if whole else f"{len(plan.keep)} из {src.ncols}"
        if plan.keep != sorted(plan.keep):
            columns += " · свой порядок"
        duplicates = {"": "не убирать", "rows": "убрать одинаковые строки",
                      "key": "оставить первую строку на каждое значение: " + src.labels(plan.key)}[plan.dedupe]
        if plan.dedupe and self.rules.soft:
            duplicates += " · без учёта регистра"
        sparse = ("оставить" if not plan.min_filled else "убрать полностью пустые" if plan.min_filled == 1
                  else f"убрать, где заполнено меньше {plan.min_filled} полей")
        required = "нет"
        if plan.require:
            glue = " и " if plan.require_any else " или "
            required = "строка убирается, если пусто: " + src.labels(plan.require, glue)
        cleaning = [name for on, name in ((plan.trim and not plan.denoise, "пробелы по краям"),
                                          (plan.denoise, "невидимые символы и лишние пробелы"),
                                          (plan.blank, "пустышки → пусто")) if on]
        layout = [encoding_name(plan.encoding), delimiter_name(plan.delimiter)]
        if src.has_header:
            layout.append("с заголовком" if plan.header else "без заголовка")
        if plan.quote_all:
            layout.append("всё в кавычках")
        changed = (plan.encoding != src.encoding or plan.delimiter != src.delimiter or plan.quote_all
                   or plan.header != src.has_header)
        return [("1", "Столбцы", columns, not whole),
                ("2", "Дубликаты", duplicates, bool(plan.dedupe)),
                ("3", "Пустые строки", sparse, bool(plan.min_filled)),
                ("4", "Обязательные", required, bool(plan.require)),
                ("5", "Битые строки", "в отдельный файл" if plan.broken == "file" else "выбросить",
                 plan.broken != "file"),
                ("6", "Значения", ", ".join(cleaning) or "без изменений", bool(cleaning)),
                ("7", "Формат", " · ".join(layout), changed),
                ("8", "Файл", clip_left(show(str(plan.path)), term_width() - 24), True)]

    def screen_export(self):
        src, plan = self.src, self.plan
        while True:
            self.page("Экспорт", "исходный файл не изменяется")
            menu(self.plan_rows())
            try:
                if shutil.disk_usage(Path(plan.path).resolve().parent).free < src.size:
                    self.tell("на диске меньше свободного места, чем занимает исходный файл", yellow)
            except OSError:
                pass
            self.foot(("Enter", "выполнить"), ("номер", "изменить"), ("0", "в меню"))
            choice = key(ask())
            if choice in ("0", "q"):
                return
            try:
                if choice == "":
                    if self.run_export():
                        return
                elif choice == "1":
                    self.pick_columns()
                elif choice == "2":
                    self.pick_dedupe()
                elif choice == "3":
                    note("убрать строки, где заполненных полей меньше чем… 1 — только пустые, 0 — не убирать")
                    text = ask()
                    if text.isdigit():
                        plan.min_filled = min(int(text), len(plan.keep))
                elif choice == "4":
                    self.pick_required()
                elif choice == "5":
                    plan.broken = "skip" if plan.broken == "file" else "file"
                elif choice == "6":
                    self.pick_cleaning()
                elif choice == "7":
                    self.pick_layout()
                elif choice == "8":
                    text = ask("новый файл ", drop=False)
                    if text:
                        path = clean_path(text)
                        plan.path = path if path.is_absolute() else src.path.parent / path
            except Back:
                pass
            except (ValueError, OSError) as error:
                self.tell(reason(error), red)

    def column_grid(self, kept):
        """Столбцы сеткой: номер, имя, заполненность; убранные — приглушены и с крестиком."""
        src, an = self.src, self.analysis
        num_w = len(str(src.ncols))
        name_w = min(max([wlen(name) for name in src.names] + [4]), 22)
        cell_w = 2 + num_w + 1 + name_w + (8 if an else 0)
        per_line = max(1, (term_width() - 2) // (cell_w + 3))
        height = -(-src.ncols // per_line)
        for y in range(height):
            cells = []
            for i in range(y, src.ncols, height):
                share = f" {pct(an.columns[i].filled, an.rows):>5} %" if an else ""
                text = f"{pad(str(i + 1), num_w, True)} {pad(src.names[i] or '—', name_w)}{share}"
                cells.append(f"  {text}" if i in kept else dim(f"× {text}"))
            print("  " + "   ".join(cells))

    def pick_columns(self):
        src, plan = self.src, self.plan
        while True:
            self.page("Экспорт › Столбцы", f"в новый файл пойдут {len(plan.keep)} из {src.ncols}")
            self.column_grid(set(plan.keep))
            if plan.keep != sorted(plan.keep):
                print()
                note("порядок в новом файле: " + " ".join(str(i + 1) for i in plan.keep))
            self.foot(("номера или имена", "убрать / вернуть"), ("=1-5 9", "оставить только эти, в этом порядке"),
                      ("<5", "убрать заполненные меньше чем на 5 %"), ("*", "все"), ("Enter", "готово"))
            text = ask()
            if not text:
                return
            try:
                if text == "*":
                    plan.keep = list(range(src.ncols))
                elif text.startswith("="):
                    plan.keep = parse_columns(text[1:], src.names) or plan.keep
                elif text.startswith("<"):
                    try:
                        limit = float(text[1:].replace(",", ".").replace("%", "").strip() or 0)
                    except ValueError:
                        raise ValueError("после «<» нужно число — процент заполненности") from None
                    if self.analysis is None:             # заполненность ещё не считалась
                        self.page("Экспорт › Столбцы")
                    an = self.analyzed()
                    kept = [i for i in plan.keep if well_filled(an, i, limit)]
                    if not kept:
                        raise ValueError("так не останется ни одного столбца")
                    self.tell(f"убрано столбцов: {len(plan.keep) - len(kept)}", green)
                    plan.keep = kept
                else:
                    picked = parse_columns(text, src.names)
                    ordered = plan.keep == sorted(plan.keep)
                    kept = [i for i in plan.keep if i not in picked] + [i for i in picked if i not in plan.keep]
                    if not kept:
                        raise ValueError("должен остаться хотя бы один столбец")
                    plan.keep = sorted(kept) if ordered else kept
            except ValueError as error:
                self.tell(str(error), red)

    def pick_dedupe(self):
        src, plan = self.src, self.plan
        self.page("Экспорт › Дубликаты")
        menu([("1", "Не убирать", ""),
              ("2", "Одинаковые строки", "из совпадающих целиком остаётся первая"),
              ("3", "По ключу", "остаётся первая строка на каждое значение выбранных столбцов")])
        if not self.rules.soft:
            self.tell("сравнение точное; без учёта регистра и пробелов по краям — в разделе «Формат»")
        self.foot()
        choice = key(ask())
        if choice == "1":
            plan.dedupe, plan.key = "", []
        elif choice == "2":
            plan.dedupe, plan.key = "rows", []
        elif choice == "3":
            picked = parse_columns(ask("столбцы ключа: номера или имена "), src.names)
            if picked:
                plan.dedupe, plan.key = "key", picked
                self.tell("строки, где ключ пуст или неполон, дублями не считаются — остаются все")

    def pick_required(self):
        src, plan = self.src, self.plan
        text = ask("столбцы, без которых строка не нужна (пусто — отменить) ")
        plan.require, plan.require_any = (parse_columns(text, src.names) if text else []), False
        if len(plan.require) > 1:
            print()
            menu([("1", "Нужны все", "строка убирается, если пуст хотя бы один"),
                  ("2", "Хватит одного", "строка убирается, только если пусты все")])
            plan.require_any = key(ask()) == "2"

    def pick_cleaning(self):
        plan = self.plan
        while True:
            self.page("Экспорт › Значения")
            trim = plan.trim or plan.denoise
            mark = {True: "вкл", False: "—  "}            # что делает пункт, видно и до включения
            menu([("1", "Пробелы по краям", mark[trim], trim, "обрезать пробелы в начале и в конце значения"),
                  ("2", "Невидимые символы", mark[plan.denoise], plan.denoise,
                   "убрать; переносы, табуляции и повторные пробелы — в один"),
                  ("3", "Пустышки", mark[plan.blank], plan.blank,
                   "значения без букв и цифр и маркеры пустоты → пустое поле")])
            self.foot(("номер", "переключить"), ("Enter", "готово"))
            choice = key(ask())
            if choice == "1":
                plan.trim, plan.denoise = not trim, False
            elif choice == "2":
                plan.denoise = not plan.denoise
            elif choice == "3":
                plan.blank = not plan.blank
            else:
                return

    def pick_layout(self):
        src, plan = self.src, self.plan
        while True:
            self.page("Экспорт › Формат нового файла")
            rows = [("1", "Кодировка", encoding_name(plan.encoding), True),
                    ("2", "Разделитель", delimiter_name(plan.delimiter), True),
                    ("3", "Кавычки", "вокруг всех значений" if plan.quote_all else "только где необходимо", True)]
            if src.has_header:
                rows.append(("4", "Заголовок", "писать" if plan.header else "не писать", True))
            menu(rows)
            self.foot(("номер", "изменить"), ("Enter", "готово"))
            choice = key(ask())
            try:
                if choice == "1":
                    text = ask("кодировка: utf-8, utf-8-sig (с BOM, для Excel), cp1251 … ")
                    plan.encoding = checked_encoding(text) if text else plan.encoding
                elif choice == "2":
                    text = ask("разделитель: символ или tab ")
                    plan.delimiter = checked_delimiter(text) if text else plan.delimiter
                elif choice == "3":
                    plan.quote_all = not plan.quote_all
                elif choice == "4" and src.has_header:
                    plan.header = not plan.header
                else:
                    return
            except ValueError as error:
                self.tell(str(error), red)

    def run_export(self):
        """Выполняет план. True — файл записан (и экран экспорта можно закрывать)."""
        src, plan = self.src, self.plan
        path = Path(plan.path)
        if path.exists():
            if os.path.samefile(path, src.path):
                self.tell("это исходный файл — его трогать нельзя; укажи другое имя в пункте 8", red)
                return False
            if not confirm(f"файл {clip_left(show(path.name), 36)} уже есть — заменить?"):
                return False
        self.page("Экспорт")
        result = export(src, self.rules, plan)
        if result.cancelled:
            self.tell("прервано — новый файл не создан", yellow)
            return False
        self.page("Экспорт › Готово", time_text(result.seconds))
        for line in outcome_lines(result, plan):
            print(line)
        print()
        print(f"  {green('→')} {show(result.path.name)}   {dim(size_text(result.size))}")
        print("    " + dim(clip_left(show(os.path.abspath(result.path.parent)), term_width() - 6)))
        self.foot(("o", "открыть новый файл здесь"), ("Enter", "в меню"))
        if key(ask()) == "o":
            self.open(result.path)
        else:
            plan.path = free_path(path)
        return True

    # ── формат исходного файла ────────────────────────────────────────────────

    def screen_format(self):
        while True:
            src, rules = self.src, self.rules

            def auto(name):               # «авто» — у того, что определено по содержимому, а не задано руками
                return "" if name in self.forced else "авто"

            self.page("Формат")
            markers = "без букв и цифр" + (", а также: " + "  ".join(rules.markers) if rules.markers else "")
            menu([("1", "Кодировка", encoding_name(src.encoding), True, auto("encoding")),
                  ("2", "Разделитель", delimiter_name(src.delimiter), True, auto("delimiter")),
                  ("3", "Кавычки", f"«{src.quote}»" if src.quote else "нет", True, auto("quote")),
                  ("4", "Экранирование", f"«{src.escape}»" if src.escape else "нет", True, auto("escape")),
                  ("5", "Заголовок", "первая строка — имена столбцов" if src.has_header
                   else "нет, данные с первой строки", True, auto("header")),
                  ("6", "Столбцов", str(src.ncols), True, auto("ncols")),
                  ("7", "Короткие строки", "чинить: дополнять пустыми полями" if rules.repair
                   else "считать битыми", True),
                  ("8", "Пустые значения", markers, True),
                  ("9", "Сравнение", "без учёта регистра и пробелов по краям" if rules.soft else "точное", True)])
            if src.note:
                self.tell(src.note, yellow)
            self.foot(("номер", "изменить"), ("a", "определить всё заново"), ("Enter", "в меню"))
            choice = key(ask())
            try:
                if choice == "1":
                    text = ask("кодировка: utf-8, cp1251, cp866, koi8-r, utf-16 …  a — авто ")
                    self.change("encoding", text, checked_encoding)
                elif choice == "2":
                    self.change("delimiter", ask("разделитель: символ или tab;  a — авто "), checked_delimiter)
                elif choice == "3":
                    self.change("quote", ask("символ кавычек;  - — кавычек нет;  a — авто "), one_char)
                elif choice == "4":
                    self.change("escape", ask("символ экранирования, обычно \\;  - — нет;  a — авто "), one_char)
                elif choice == "5":
                    self.change("header", "x", lambda _: not src.has_header)
                elif choice == "6":
                    self.change("ncols", ask("сколько полей должно быть в строке;  a — авто "), checked_count)
                elif choice == "7":
                    self.rules = Rules(rules.markers, rules.soft, not rules.repair)
                elif choice == "8":
                    text = ask("маркеры пустоты через запятую;  - — без маркеров ")
                    if text:
                        self.rules = Rules(() if text == "-" else text.split(","), rules.soft, rules.repair)
                elif choice == "9":
                    self.rules = Rules(rules.markers, not rules.soft, rules.repair)
                elif choice == "a":
                    self.forced.clear()
                    self.reload()
                else:
                    return
            except (ValueError, OSError) as error:
                self.tell(f"не подходит — {reason(error)}", red)

    def change(self, name, text, convert):
        """Параметр формата: пустой ввод — не менять, «a» — определять автоматически, иначе — задать."""
        if not text:
            return
        before = dict(self.forced)
        if key(text) == "a":
            self.forced.pop(name, None)
        else:
            self.forced[name] = convert(text)
        try:
            self.reload()
        except (ValueError, OSError, LookupError):
            self.forced = before
            self.reload()
            raise

    def reload(self):
        columns = self.src.ncols
        src = Source(self.src.path, self.forced)
        if not src.ncols:
            raise ValueError("с такими настройками в файле не видно ни одного столбца")
        self.src, self.analysis = src, None
        if src.ncols != columns:
            self.plan = Plan(src)
        else:
            self.plan.encoding, self.plan.delimiter, self.plan.header = src.encoding, src.delimiter, src.has_header


# ══════════════════════════════════════════════════════════════════════════════
#  КОМАНДНАЯ СТРОКА
# ══════════════════════════════════════════════════════════════════════════════

class _Help(argparse.RawDescriptionHelpFormatter):
    def add_usage(self, usage, actions, groups, prefix=None):
        super().add_usage(usage, actions, groups, "запуск: ")


def build_parser():
    me = Path(sys.argv[0]).name or "tdb.py"
    parser = argparse.ArgumentParser(
        prog=me, add_help=False, formatter_class=_Help, usage=f"{me} [файл] [опции]",
        description="Анализ и очистка текстовых баз (CSV, TSV, TXT). Без -a / -r / -o — интерактивный режим:\n"
                    "файл базы достаточно перетащить в окно.",
        epilog="СТОЛБЦЫ — номера с единицы, диапазоны, имена и маски имён: 1,3-5,email,addr*\n\n"
               "примеры:\n"
               f"  {me} base.csv -a\n"
               f"  {me} base.csv -r base.json\n"
               f"  {me} base.csv -o new.csv --drop 3,7-12 --dedupe\n"
               f"  {me} base.csv -o new.csv --keep email,phone --dedupe email --require email --trim\n"
               f"  {me} base.csv -o new.csv --drop-empty 5 --min-filled 1 --out-encoding utf-8")
    parser.add_argument("file", nargs="?", help=argparse.SUPPRESS)
    group = parser.add_argument_group("действие")
    group.add_argument("-a", "--analyze", action="store_true", help="показать заполненность и уникальность столбцов")
    group.add_argument("-r", "--report", metavar="ФАЙЛ", help="сохранить отчёт анализа: .txt, .csv или .json")
    group.add_argument("-o", "--out", metavar="ФАЙЛ", help="записать преобразованную базу в новый файл")
    group = parser.add_argument_group("формат исходного файла (по умолчанию определяется сам)")
    group.add_argument("-e", "--encoding", metavar="ИМЯ", help="кодировка: utf-8, cp1251, cp866 …")
    group.add_argument("-d", "--delimiter", metavar="СИМВОЛ", help="разделитель; tab — табуляция")
    group.add_argument("-q", "--quote", metavar="СИМВОЛ", help="символ кавычек; none — кавычек нет")
    group.add_argument("--escape", metavar="СИМВОЛ", help="символ экранирования, обычно \\; none — нет")
    group.add_argument("--header", choices=("yes", "no"), help="есть ли строка заголовков")
    group.add_argument("--columns", type=int, metavar="N", help="сколько полей должно быть в строке")
    group.add_argument("--repair", action="store_true",
                       help="короткие строки дополнять пустыми полями, а не считать битыми")
    group.add_argument("--nulls", metavar="СПИСОК", help="маркеры пустоты через запятую; none — без маркеров")
    group.add_argument("-i", "--ignore-case", action="store_true",
                       help="сравнивать без учёта регистра и пробелов по краям")
    group = parser.add_argument_group("экспорт (вместе с -o)")
    group.add_argument("-k", "--keep", metavar="СТОЛБЦЫ", help="оставить только эти столбцы, в этом порядке")
    group.add_argument("-x", "--drop", metavar="СТОЛБЦЫ", help="убрать столбцы")
    group.add_argument("--drop-empty", metavar="%", nargs="?", const=0.0, type=float,
                       help="убрать столбцы, заполненные меньше чем на %% (без числа — полностью пустые)")
    group.add_argument("-u", "--dedupe", metavar="СТОЛБЦЫ", nargs="?", const="*",
                       help="убрать дубликаты: строки целиком или по столбцам-ключу")
    group.add_argument("--min-filled", type=int, metavar="N",
                       help="убрать строки, где заполнено меньше N полей (1 — пустые)")
    group.add_argument("--require", metavar="СТОЛБЦЫ", help="убрать строки, где пуст хотя бы один из этих столбцов")
    group.add_argument("--require-any", metavar="СТОЛБЦЫ", help="убрать строки, где пусты все эти столбцы")
    group.add_argument("--trim", action="store_true", help="обрезать пробелы по краям значений")
    group.add_argument("--denoise", action="store_true", help="убрать невидимые символы, переносы и повторные пробелы")
    group.add_argument("--blank", action="store_true", help="значения без данных и маркеры пустоты → пустое поле")
    group.add_argument("--broken", choices=("file", "skip"), default="file",
                       help="битые строки: file — в отдельный файл (по умолчанию), skip — выбросить")
    group.add_argument("--out-encoding", metavar="ИМЯ", help="кодировка нового файла (по умолчанию как в исходном)")
    group.add_argument("--out-delimiter", metavar="СИМВОЛ", help="разделитель нового файла")
    group.add_argument("--no-out-header", action="store_true", help="не писать строку заголовков")
    group.add_argument("--quote-all", action="store_true", help="брать в кавычки все значения")
    group.add_argument("-y", "--yes", action="store_true", help="заменить файл результата, если он уже есть")
    group = parser.add_argument_group("прочее")
    group.add_argument("-h", "--help", action="help", help="показать эту справку")
    group.add_argument("--version", action="version", version=f"tdb {__version__}", help="показать версию")
    return parser


def forced_from(args):
    """Параметры формата, заданные в командной строке."""
    forced = {}
    if args.encoding:
        forced["encoding"] = checked_encoding(args.encoding)
    if args.delimiter:
        forced["delimiter"] = checked_delimiter(args.delimiter)
    if args.quote:
        forced["quote"] = one_char(args.quote)
    if args.escape:
        forced["escape"] = one_char(args.escape)
    if args.header:
        forced["header"] = args.header == "yes"
    if args.columns:
        forced["ncols"] = args.columns
    return forced


def batch(args, src, rules):
    """Работа без вопросов: анализ и/или экспорт по опциям командной строки."""
    an = None
    print()
    file_card(src)
    if args.analyze or args.report or args.drop_empty is not None:
        an = analyze(src, rules)
        if args.analyze:
            print()
            for line in table_lines(an) + [""] + summary_lines(an):
                print(line)
        if args.report:
            print(f"\n  {green('отчёт сохранён →')} {save_report(src, an, args.report)}")
        if an.part < 1:
            return 130
    if not args.out:
        print()
        return 0

    plan = Plan(src)
    plan.path = Path(args.out)
    if plan.path.exists() and not os.path.samefile(plan.path, src.path) and not args.yes:
        raise ValueError(f"файл {plan.path} уже есть; -y — заменить")
    if args.keep:
        plan.keep = parse_columns(args.keep, src.names)
    if args.drop:
        dropped = set(parse_columns(args.drop, src.names))
        plan.keep = [i for i in plan.keep if i not in dropped]
    if args.drop_empty is not None:
        plan.keep = [i for i in plan.keep if well_filled(an, i, args.drop_empty)]
    if args.dedupe:
        plan.dedupe = "rows" if args.dedupe == "*" else "key"
        plan.key = [] if args.dedupe == "*" else parse_columns(args.dedupe, src.names)
    plan.min_filled = max(0, args.min_filled or 0)
    if args.require or args.require_any:
        plan.require = parse_columns(args.require or args.require_any, src.names)
        plan.require_any = not args.require
    plan.trim, plan.denoise, plan.blank, plan.broken = args.trim, args.denoise, args.blank, args.broken
    plan.quote_all = args.quote_all
    plan.header = src.has_header and not args.no_out_header
    if args.out_encoding:
        plan.encoding = checked_encoding(args.out_encoding)
    if args.out_delimiter:
        plan.delimiter = checked_delimiter(args.out_delimiter)

    result = export(src, rules, plan)
    print()
    if result.cancelled:
        alert("прервано — новый файл не создан")
        return 130
    for line in outcome_lines(result, plan):
        print(line)
    print(f"\n  {green('→')} {result.path}   {dim(size_text(result.size) + ' · ' + time_text(result.seconds))}\n")
    return 0


def main(argv=None):
    args = build_parser().parse_args(argv)
    setup_console()
    csv.field_size_limit(min(FIELD_LIMIT_MB << 20, 2 ** 31 - 1))
    try:
        markers = NULL_MARKERS
        if args.nulls is not None:
            markers = () if args.nulls.lower() == "none" else args.nulls.split(",")
        rules = Rules(markers, args.ignore_case or IGNORE_CASE, args.repair or REPAIR_ROWS)
        forced = forced_from(args)
        path = args.file or DEFAULT_FILE
        if args.analyze or args.report or args.out:
            if not path:
                raise ValueError("не указан файл")
            src = Source(clean_path(path), forced)
            if not src.ncols:
                raise ValueError("файл пуст или в нём нет строк с данными")
            return batch(args, src, rules)
        App(rules, forced).run(path)
        return 0
    except KeyboardInterrupt:
        print()
        return 130
    except (OSError, ValueError, LookupError) as error:
        fail(reason(error))
        return 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception:                     # непредвиденное: показать, а не дать окну закрыться молча
        import traceback
        traceback.print_exc()
        if UI.interactive and sys.stdin.isatty():
            try:
                input("\n  Что-то сломалось — текст ошибки выше. Enter — закрыть ")
            except (EOFError, KeyboardInterrupt):
                pass
        sys.exit(1)
