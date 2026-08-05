#!/usr/bin/env bash
# Снимок ЖИВОГО окна bmv-gui на раннере Linux (дисплея у него нет — поднимаем свой).
#
# Что происходит по шагам:
#   1. Xvfb — виртуальный экран 1280×1024. Окно у нас 400×779 точек (см.
#      window_size/fit_height в apps/bmv-gui/src/main.rs), помещается с запасом.
#   2. Локальный координатор тем же бинарём `bemyvpn server` + ДВА настоящих
#      хоста `bemyvpn host`. Иначе каталог пуст и снимок показывает половину
#      приложения. Хосты ходят через tools/ci-xff-relay.py — почему, написано там.
#   3. Каталог проверяем ДО запуска окна (`bemyvpn guest`): нет хостов — падаем
#      сразу и с понятной причиной, а не выкладываем пустой список как «проверку».
#   4. Ждём появления окна, даём ему дорисоваться, снимаем и САМО ОКНО, и весь
#      экран. Кадр проверяем на осмысленность (число цветов) — серое
#      неотрисованное окно обязано КРАСНИТЬ задачу, а не уезжать в артефакт.
#
# Права администратора НЕ НУЖНЫ: на Unix их спрашивает только помощник туннеля в
# момент «Подключить» (apps/bmv-gui/src/helper.rs), а мы ничего не подключаем.
set -euo pipefail

OUT="${1:-shot}"
BIN_GUI="target/release/bemyvpn-gui"
BIN_CLI="target/release/bemyvpn"
TMP="$(mktemp -d)"
mkdir -p "$OUT"

PIDS=()
cleanup() {
    for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done
    # Журналы участников — в артефакт: разбирать красный прогон иначе не по чему.
    cp "$TMP"/*.log "$OUT"/ 2>/dev/null || true
}
trap cleanup EXIT

# ── 1. Виртуальный экран ─────────────────────────────────────────────────────
Xvfb :99 -screen 0 1280x1024x24 -nolisten tcp >"$TMP/xvfb.log" 2>&1 &
PIDS+=($!)
export DISPLAY=:99
# Софтверный OpenGL (llvmpipe): femtovg — GPU-рендерер, а видеокарты у раннера нет.
export LIBGL_ALWAYS_SOFTWARE=1
for _ in $(seq 1 50); do xdpyinfo >/dev/null 2>&1 && break; sleep 0.2; done
xdpyinfo | head -3

# ── 2. Локальный координатор + подстава внешнего адреса + два хоста ──────────
printf '[host]\npublic = true\nmax_guests = 8\n' >"$TMP/host.toml"
printf 'coordinators = ["http://127.0.0.1:3330"]\n' >"$TMP/gui.toml"

"$BIN_CLI" server --bind 127.0.0.1:3330 >"$TMP/coord.log" 2>&1 &
PIDS+=($!)
python3 tools/ci-xff-relay.py 3330 3331:8.8.8.8 3332:77.88.55.88 >"$TMP/relay.log" 2>&1 &
PIDS+=($!)
sleep 2

"$BIN_CLI" --config "$TMP/host.toml" --coordinator http://127.0.0.1:3331 \
    host --name "Хост-один (CI)" >"$TMP/host1.log" 2>&1 &
PIDS+=($!)
"$BIN_CLI" --config "$TMP/host.toml" --coordinator http://127.0.0.1:3332 \
    host --name "Хост-два (CI)" >"$TMP/host2.log" 2>&1 &
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
# Каталог доезжает до окна первым снимком (в bmv-signal на него 6 с), плюс время
# на отрисовку софтверным GL. Снимать раньше — снимать заготовку.
sleep 10

import -window "$WID" "$OUT/linux-okno.png"
import -window root "$OUT/linux-ekran.png"

# ── 5. Кадр обязан быть осмысленным ──────────────────────────────────────────
read -r W H COLORS < <(identify -format '%w %h %k' "$OUT/linux-okno.png")
echo "снимок окна: ${W}x${H}, уникальных цветов: $COLORS"
if [ "$COLORS" -lt 200 ]; then
    echo "ОШИБКА: в кадре $COLORS цветов — окно не отрисовалось." >&2
    exit 1
fi
