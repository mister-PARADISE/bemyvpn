#!/usr/bin/env bash
# Собирает single-file AppImage из уже собранного target/release/bemyvpn-gui.
# Это «APK для Linux»: ОДИН файл, chmod +x, запуск — без apt/сервисов/зависимостей
# (нужные либы вшиты, системные X11/Wayland есть на любом десктопе).
#
# Требует на СБОРОЧНОЙ машине: rsvg-convert (librsvg2-bin), wget. Запуск:
#   packaging/linux/build-appimage.sh [OUT_DIR]
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
BIN="$ROOT/target/release/bemyvpn-gui"
OUT="${1:-$ROOT/dist}"
VERSION="${VERSION:-0.49}"
export APPIMAGE_EXTRACT_AND_RUN=1   # инструменты-AppImage без FUSE (headless VPS)

[ -x "$BIN" ] || { echo "НЕТ бинаря $BIN — сначала: cargo build --release -p bmv-gui"; exit 1; }
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"   # абсолютный путь: ниже делаем cd в temp, относительный сломался бы
WORK="$(mktemp -d)"; trap 'rm -rf "$WORK"' EXIT
APP="$WORK/AppDir"
mkdir -p "$APP/usr/bin" "$APP/usr/share/applications" "$APP/usr/share/icons/hicolor/256x256/apps"

cp "$BIN" "$APP/usr/bin/bemyvpn-gui"
cp "$HERE/bemyvpn.desktop" "$APP/usr/share/applications/bemyvpn.desktop"
rsvg-convert -w 256 -h 256 "$HERE/bemyvpn.svg" \
  -o "$APP/usr/share/icons/hicolor/256x256/apps/bemyvpn.png"

cd "$WORK"
get() { wget -q "$1" -O "$2" && chmod +x "$2"; }
get https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-x86_64.AppImage linuxdeploy
get https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage appimagetool

# libxkbcommon и libxkbcommon-x11 грузятся через dlopen (крейт xkbcommon-dl),
# поэтому ldd их НЕ видит и linuxdeploy сам не вшивает. Без них на минимальных
# системах приложение падает «libxkbcommon-x11.so could not be loaded». Добавляем
# принудительно через --library. Остальные X11/Wayland — прямые зависимости,
# linuxdeploy подхватит их по ldd.
FORCE_LIBS=()
for name in libxkbcommon.so.0 libxkbcommon-x11.so.0; do
  p="$(ldconfig -p | grep -m1 "$name" | awk '{print $NF}')"
  [ -n "$p" ] && FORCE_LIBS+=(--library "$p")
done

./linuxdeploy --appdir "$APP" \
  --desktop-file "$APP/usr/share/applications/bemyvpn.desktop" \
  --icon-file "$APP/usr/share/icons/hicolor/256x256/apps/bemyvpn.png" \
  "${FORCE_LIBS[@]}"

ARCH=x86_64 ./appimagetool "$APP" "$OUT/bemyvpn-linux-x86_64.AppImage"
echo "ГОТОВ: $OUT/bemyvpn-linux-x86_64.AppImage ($(du -h "$OUT/bemyvpn-linux-x86_64.AppImage" | cut -f1))"
