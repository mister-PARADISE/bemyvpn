#!/usr/bin/env bash
# Снимки ЖИВОГО окна bmv-gui на раннере Linux — по одному на каждую вкладку.
#
# Что происходит по шагам:
#   1. Xvfb — виртуальный экран 1280×1024. Окно у нас 400×779 точек (см.
#      window_size/fit_height в apps/bmv-gui/src/main.rs), помещается с запасом.
#   2. Оконный менеджер fluxbox и цвет фона. Голый Xvfb — это НЕ рабочий стол:
#      без менеджера окно приходит без заголовка и прибивается в угол чёрного
#      поля, а просили показать приложение «в жизни». fluxbox выбран из того,
#      что есть в apt, по трём причинам: рисует настоящий заголовок с кнопками,
#      сам приносит панель задач снизу (openbox и jwm её не дают, а без панели
#      кадр всё равно не читается как рабочий стол) и поднимается за долю
#      секунды на ~2 МБ. Фон красит xsetroot из x11-xserver-utils — картинку
#      тащить некуда, а сплошной цвет уже отличает стол от пустоты.
#   3. Локальный координатор тем же бинарём `bemyvpn server` + ДВА настоящих
#      хоста `bemyvpn host`. Иначе каталог пуст и снимок показывает половину
#      приложения. Хосты ходят через tools/ci-xff-relay.py — почему, написано там.
#   4. Каталог проверяем ДО запуска окна (`bemyvpn guest`): нет хостов — падаем
#      сразу и с понятной причиной, а не выкладываем пустой список как «проверку».
#   5. Окно двигаем в центр стола и снимаем ВЕСЬ ЭКРАН — рабочий стол, заголовок
#      и панель задач в кадре. Вырезка одного окна показывала приложение как
#      картинку из макета, а не как программу на чужой машине.
#   6. Вкладки переключаем НАСТОЯЩИМ щелчком по нав-бару, куда считает
#      tools/ci-shot-check.py, и он же каждый кадр проверяет.
#
# ПОРЯДОК ВКЛАДОК НЕ СЛУЧАЕН: VPN → Хост → Сервер.
#   * VPN открыта при запуске, поэтому первый кадр берём без щелчка.
#   * Щёлкаем только по ячейкам ЧУЖИХ вкладок. Ячейка своей вкладки — это уже не
#     навигация, а включатель («Старт», «Раздать», см. ui/app.slint), и щелчок по
#     ней запустил бы подключение вместо переключения.
#   * «Сервер» — последней намеренно. Под программным растеризатором эта вкладка
#     роняет процесс в момент обновления списка недавних серверов (чужая поломка
#     Slint, разобрана в restart_on_software_renderer). Обновление это разовое и
#     случается в первые секунды после запуска — то есть пока мы снимаем две
#     первые вкладки. Приходить на «Сервер» позже безопаснее всего.
#
# Права администратора НЕ НУЖНЫ: на Unix их спрашивает только помощник туннеля в
# момент «Подключить» (apps/bmv-gui/src/helper.rs), а мы ничего не подключаем.
set -euo pipefail

OUT="${1:-shot}"
BIN_GUI="target/release/bemyvpn-gui"
BIN_CLI="target/release/bemyvpn"
CHECK="tools/ci-shot-check.py"
SCREEN_W=1280
SCREEN_H=1024
TMP="$(mktemp -d)"
mkdir -p "$OUT"

PIDS=()
cleanup() {
    for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done
    # Журналы участников — в артефакт: разбирать красный прогон иначе не по чему.
    cp "$TMP"/*.log "$OUT"/ 2>/dev/null || true
}
trap cleanup EXIT

# ── 1. Виртуальный экран, оконный менеджер, фон ──────────────────────────────
Xvfb :99 -screen 0 "${SCREEN_W}x${SCREEN_H}x24" -nolisten tcp >"$TMP/xvfb.log" 2>&1 &
PIDS+=($!)
export DISPLAY=:99
# Софтверный OpenGL (llvmpipe): femtovg — GPU-рендерер, а видеокарты у раннера нет.
export LIBGL_ALWAYS_SOFTWARE=1
for _ in $(seq 1 50); do xdpyinfo >/dev/null 2>&1 && break; sleep 0.2; done
xdpyinfo | head -3

xsetroot -solid '#243040'
fluxbox >"$TMP/fluxbox.log" 2>&1 &
PIDS+=($!)
sleep 3

# ── 2. Локальный координатор + подстава внешнего адреса + два хоста ──────────
printf '[host]\npublic = true\nmax_guests = 8\n' >"$TMP/host.toml"
printf 'coordinators = ["http://127.0.0.1:3330"]\n' >"$TMP/gui.toml"

"$BIN_CLI" server --bind 127.0.0.1:3330 >"$TMP/coord.log" 2>&1 &
PIDS+=($!)
python3 tools/ci-xff-relay.py 3330 3331:8.8.8.8 3332:77.88.55.88 >"$TMP/relay.log" 2>&1 &
PIDS+=($!)
sleep 2

# Имена кириллицей — намеренно: их отрисовка и есть часть проверки. Пробелов в
# имени НЕТ: на Windows тот же запуск идёт через Start-Process, который склеивает
# аргументы без кавычек, и «Хост-1 (CI)» приезжало двумя аргументами. Имена на
# обеих ОС обязаны совпадать, иначе снимки не сравнить.
"$BIN_CLI" --config "$TMP/host.toml" --coordinator http://127.0.0.1:3331 \
    host --name "Хост-1(CI)" >"$TMP/host1.log" 2>&1 &
PIDS+=($!)
"$BIN_CLI" --config "$TMP/host.toml" --coordinator http://127.0.0.1:3332 \
    host --name "Хост-2(CI)" >"$TMP/host2.log" 2>&1 &
PIDS+=($!)

# ── 3. Каталог обязан быть непустым ──────────────────────────────────────────
for _ in $(seq 1 20); do
    "$BIN_CLI" --coordinator http://127.0.0.1:3330 guest >"$TMP/dir.log" 2>&1 || true
    if [ "$(grep -c '(CI)' "$TMP/dir.log" || true)" -ge 2 ]; then break; fi
    sleep 1
done
cat "$TMP/dir.log"
if [ "$(grep -c '(CI)' "$TMP/dir.log" || true)" -lt 2 ]; then
    echo "ОШИБКА: хосты не доехали до каталога — снимать нечего." >&2
    cat "$TMP/host1.log" "$TMP/host2.log" >&2
    exit 1
fi

# ── 4. Окно ──────────────────────────────────────────────────────────────────
BEMYVPN_CONFIG="$TMP/gui.toml" "$BIN_GUI" >"$TMP/gui.log" 2>&1 &
PIDS+=($!)

WID=""
for _ in $(seq 1 60); do
    WID="$(xdotool search --name '^BeMyVPN$' 2>/dev/null | head -1 || true)"
    if [ -n "$WID" ]; then break; fi
    sleep 1
done
if [ -z "$WID" ]; then
    echo "ОШИБКА: окно так и не появилось за 60 с." >&2
    cat "$TMP/gui.log" >&2
    exit 1
fi
echo "окно найдено: $WID"

# Ставим окно в середину стола. Смещать ОБЯЗАТЕЛЬНО, а не полагаться на
# менеджер: fluxbox кладёт первое окно в левый верхний угол, и кадр выходит
# «программа в углу», а не «программа на столе».
eval "$(xdotool getwindowgeometry --shell "$WID")"
xdotool windowmove "$WID" $(( (SCREEN_W - WIDTH) / 2 )) $(( (SCREEN_H - HEIGHT) / 2 ))
xdotool windowactivate "$WID" || true
sleep 2
# Читаем положение ЗАНОВО: заголовок, который дорисовал менеджер, сдвигает
# клиентскую область вниз, и считать координаты щелчка по задуманному значению
# значит мазать мимо бара ровно на высоту заголовка.
eval "$(xdotool getwindowgeometry --shell "$WID")"
echo "клиентская область: ${WIDTH}x${HEIGHT} в ${X},${Y}"
# Масштаба экрана у Xvfb нет — точка равна пикселю.
RECT="$X $Y $WIDTH $HEIGHT 1"

# Каталог доезжает до окна первым снимком (в bmv-signal на него 6 с), плюс время
# на отрисовку софтверным GL. Снимать раньше — снимать заготовку.
sleep 10

# ── 5. Три вкладки ───────────────────────────────────────────────────────────
shoot() {   # $1 — номер вкладки, $2 — имя файла
    # Курсор уводим с бара: он и сам может попасть в кадр, и держать его на
    # ячейке значит мерить подсвеченной не ту, что открыта, а ту, что под мышью.
    xdotool mousemove 20 20
    sleep 3
    import -window root "$OUT/linux-$2.png"
    python3 "$CHECK" check "$OUT/linux-$2.png" $RECT "$1"
}

open_tab() {   # $1 — номер вкладки; щёлкаем ровно в центр её ячейки
    local xy
    xy="$(python3 "$CHECK" coords $RECT "$1")"
    echo "щелчок по вкладке $1 в $xy"
    xdotool mousemove $xy
    sleep 1
    xdotool click 1
    sleep 2
}

shoot 0 vpn
open_tab 1
shoot 1 host
open_tab 2
shoot 2 server

python3 "$CHECK" distinct "$OUT/linux-vpn.png" "$OUT/linux-host.png" "$OUT/linux-server.png" $RECT
