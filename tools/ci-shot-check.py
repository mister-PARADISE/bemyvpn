#!/usr/bin/env python3
# Разметка нав-бара и проверка снимков — ОДИН исходник на Windows и Linux.
#
# ПОЧЕМУ ОБЩИЙ ФАЙЛ, А НЕ ПО КУСКУ В КАЖДОЙ ОБОЛОЧКЕ. Куда щёлкнуть мышью и как
# потом убедиться, что открылась именно та вкладка, — это ОДНА И ТА ЖЕ разметка
# плавающего бара из ui/app.slint. Написанная дважды (bash+ImageMagick и
# PowerShell+System.Drawing) она разошлась бы при первой правке разметки, и одна
# из двух ОС молча проверяла бы не то. Здесь она описана один раз, а обе
# оболочки её спрашивают — и щёлкают, и проверяют по одному и тому же числу.
#
# ВЕСЬ ПЕЧАТАЕМЫЙ ВЫВОД — ЛАТИНИЦЕЙ. Скрипт зовут и с Windows-раннера, где
# кириллица в консоли уже роняла задачу ошибкой кодировки.
#
# Все размеры снаружи приходят в ЭКРАННЫХ ПИКСЕЛЯХ (прямоугольник клиентской
# области окна) плюс масштаб экрана; внутри всё считается в ЛОГИЧЕСКИХ ТОЧКАХ —
# тех же, в которых написана разметка Slint.
import sys

from PIL import Image

# ── Разметка плавающего бара: ui/app.slint, `bar := FloatCard` ───────────────
BAR_MX = 18.0      # отступ бара от боковых краёв окна
BAR_BOTTOM = 30.0  # подъём бара над нижним краем окна
BAR_H = 66.0       # высота бара
PAD = 6.0          # padding у HorizontalLayout внутри бара
GAP = 4.0          # spacing между ячейками
# Слева направо в баре стоят вкладки в таком порядке — не по номеру.
ORDER = (2, 0, 1)  # Сервер | VPN | Хост
NAMES = {0: "vpn", 1: "host", 2: "server"}

# ── Пороги проверок ──────────────────────────────────────────────────────────
# Насколько активная ячейка «зеленее» спящих. Активная залита
# Theme.picked(accent) = card.mix(accent, 0.845) ≈ RGB(22,52,47), то есть G−R =
# +30; спящая показывает фон бара #1A1E24, у него G−R = +4. Порог 8 стоит вдвое
# выше шума спящей ячейки и вчетверо ниже ожидаемой разницы: и подделать нечем,
# и на случайной букве не сорвётся.
GREEN_MARGIN = 8.0
# Столько разных цветов обязано быть в области страницы. Серый неотрисованный
# прямоугольник даёт единицы, живая страница со сглаженным текстом — тысячи.
MIN_COLORS = 200
# Такая доля точек обязана отличаться между двумя вкладками. Меньше — значит
# бар подсветку переставил, а страница под ним осталась прежней: замёрзла.
MIN_DIFF = 0.10
# Порог «точка отличается»: ниже него разницу даёт одно лишь сглаживание.
DIFF_LEVEL = 24


def cells(w, h, dpi):
    """Ячейки нав-бара в логических точках: {номер вкладки: (x, y, w, h)}."""
    wl, hl = w / dpi, h / dpi
    barw = wl - 2 * BAR_MX
    cw = (barw - 2 * PAD - 2 * GAP) / 3.0
    # Ячейки живут внутри padding бара, поэтому +PAD и по x, и по y.
    cy = hl - BAR_BOTTOM - BAR_H + PAD
    ch = BAR_H - 2 * PAD
    return {t: (BAR_MX + PAD + i * (cw + GAP), cy, cw, ch) for i, t in enumerate(ORDER)}


def page(w, h, dpi):
    """Видимая область страницы: всё окно выше бара (бар плавает ПОВЕРХ неё)."""
    return (0.0, 0.0, w / dpi, h / dpi - BAR_BOTTOM - BAR_H - PAD)


def crop(img, ox, oy, dpi, box, inset=0.0):
    """Кусок снимка по логическому прямоугольнику окна.

    Отступ внутрь (`inset`) нужен ячейкам: у них рамка в 1 точку и скруглённые
    углы, и без запаса в замер лезли бы точки соседнего слоя.
    """
    x, y, bw, bh = box
    left = int(round(ox + dpi * (x + inset)))
    top = int(round(oy + dpi * (y + inset)))
    right = int(round(ox + dpi * (x + bw - inset)))
    bottom = int(round(oy + dpi * (y + bh - inset)))
    if left < 0 or top < 0 or right > img.width or bottom > img.height or right <= left or bottom <= top:
        die("window area {},{} {}x{} does not fit the {}x{} screenshot".format(
            left, top, right - left, bottom - top, img.width, img.height))
    return img.crop((left, top, right, bottom)).convert("RGB")


def greenness(im):
    """Средний перевес зелёного над красным — метка залитой мятой ячейки."""
    data = list(im.getdata())
    return sum(g - r for r, g, _ in data) / float(len(data))


def die(msg):
    print("FAIL: " + msg)
    sys.exit(1)


def rect_args(argv):
    """X Y W H DPI — прямоугольник клиентской области окна на экране."""
    x, y, w, h = (int(v) for v in argv[:4])
    return x, y, w, h, float(argv[4])


def cmd_coords(argv):
    """Куда ткнуть мышью, чтобы открыть вкладку: экранные пиксели центра ячейки."""
    x, y, w, h, dpi = rect_args(argv)
    cx, cy, cw, ch = cells(w, h, dpi)[int(argv[5])]
    print("%d %d" % (round(x + dpi * (cx + cw / 2)), round(y + dpi * (cy + ch / 2))))


def cmd_check(argv):
    """Снимок обязан показывать живое окно с ОТКРЫТОЙ ИМЕННО ЭТОЙ вкладкой."""
    img = Image.open(argv[0])
    x, y, w, h, dpi = rect_args(argv[1:])
    tab = int(argv[6])

    # 1. Страница не пуста. Проверяем область СТРАНИЦЫ, а не всё окно: бар
    #    рисуется всегда и своими цветами вытянул бы даже мёртвое окно за порог.
    body = crop(img, x, y, dpi, page(w, h, dpi))
    colors = len(set(body.getdata()))
    print("page area %dx%d, distinct colours %d" % (body.width, body.height, colors))
    if colors < MIN_COLORS:
        die("only %d distinct colours on the page - the app did not paint" % colors)

    # 2. Подсвечена ЗАДАННАЯ ячейка бара. Это и есть честный ответ на вопрос
    #    «открыта ли нужная вкладка»: сравниваем не наше намерение, а то, что
    #    приложение НАРИСОВАЛО. Не долетел щелчок, замёрзло окно, поменялся
    #    порядок ячеек — любая из этих бед красит задачу.
    boxes = cells(w, h, dpi)
    green = {t: greenness(crop(img, x, y, dpi, b, inset=6.0)) for t, b in boxes.items()}
    print("navbar green-red: " + ", ".join(
        "%s %.1f" % (NAMES[t], green[t]) for t in ORDER))
    rivals = max(green[t] for t in green if t != tab)
    if green[tab] - rivals < GREEN_MARGIN:
        die("tab '%s' is not the lit one (%.1f vs %.1f, need +%.1f)" % (
            NAMES[tab], green[tab], rivals, GREEN_MARGIN))
    print("OK: tab '%s' is open" % NAMES[tab])


def cmd_distinct(argv):
    """Три страницы обязаны РАЗЛИЧАТЬСЯ.

    Подсветка в баре доказывает, что щелчок дошёл; а что под баром сменилась
    сама страница — доказывает только сравнение самих страниц. Замёрзшее окно с
    живым баром иначе проехало бы как три разные вкладки.
    """
    x, y, w, h, dpi = rect_args(argv[3:])
    box = page(w, h, dpi)
    shots = [(p, list(crop(Image.open(p), x, y, dpi, box).getdata())) for p in argv[:3]]
    for i in range(3):
        for j in range(i + 1, 3):
            a, b = shots[i][1], shots[j][1]
            n = sum(1 for p, q in zip(a, b) if max(abs(p[0] - q[0]), abs(p[1] - q[1]), abs(p[2] - q[2])) > DIFF_LEVEL)
            share = n / float(len(a))
            print("%s vs %s: %.1f%% of pixels differ" % (shots[i][0], shots[j][0], 100 * share))
            if share < MIN_DIFF:
                die("pages %s and %s look the same - the window is frozen" % (shots[i][0], shots[j][0]))
    print("OK: three different pages")


def cmd_selftest(_argv):
    """Проверка самой проверки: она обязана КРАСНЕТЬ на чужой вкладке.

    Зелёная задача при неработающем приложении хуже красной, поэтому мало
    написать проверку — надо показать, что она умеет отказать. Рисуем
    поддельный «снимок экрана» с баром, где подсвечена одна известная ячейка, и
    требуем: за неё проверка ручается, за две другие — нет.
    """
    import os
    import random
    import tempfile

    scr_w, scr_h, ox, oy, w, h = 1280, 1024, 440, 122, 400, 779
    rnd = random.Random(1)
    img = Image.new("RGB", (scr_w, scr_h), (36, 48, 64))
    px = img.load()
    # Страница: шум вместо содержимого — важно лишь, что цветов много.
    for yy in range(oy, oy + h):
        for xx in range(ox, ox + w):
            px[xx, yy] = (rnd.randrange(64), rnd.randrange(64), rnd.randrange(64))
    lit = 1
    for tab, (cx, cy, cw, ch) in cells(w, h, 1.0).items():
        fill = (22, 52, 47) if tab == lit else (26, 30, 36)
        for yy in range(int(oy + cy), int(oy + cy + ch)):
            for xx in range(int(ox + cx), int(ox + cx + cw)):
                px[xx, yy] = fill

    path = os.path.join(tempfile.mkdtemp(), "fake.png")
    img.save(path)
    rect = [str(ox), str(oy), str(w), str(h), "1"]
    for tab in (0, 1, 2):
        try:
            cmd_check([path] + rect + [str(tab)])
            verdict = "passed"
        except SystemExit:
            verdict = "refused"
        want = "passed" if tab == lit else "refused"
        assert verdict == want, "tab %d: %s, expected %s" % (tab, verdict, want)
    print("OK: the check passes the lit tab and refuses the other two")


if __name__ == "__main__":
    {
        "coords": cmd_coords,
        "check": cmd_check,
        "distinct": cmd_distinct,
        "selftest": cmd_selftest,
    }[sys.argv[1]](sys.argv[2:])
