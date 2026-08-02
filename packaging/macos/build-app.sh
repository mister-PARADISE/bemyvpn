#!/usr/bin/env bash
# Собирает BeMyVPN.app из target/release/bemyvpn-gui, кладёт нормальную иконку,
# подписывает бандл и пакует в .dmg («один файл»: открыл → перетащил в Программы).
#
# ПОДПИСЬ — два режима, выбирается автоматически по наличию сертификата:
#   • Есть Developer ID (env MACOS_SIGN_ID) → «настоящая» подпись + hardened runtime,
#     а если заданы креды нотаризации — ещё нотаризация + staple → двойной клик без
#     правого клика и «неизвестного разработчика». (Готово к покупке Apple-подписи.)
#   • Нет сертификата → АД-ХОК подпись (как сейчас): на Apple Silicon .app не
#     «повреждён», открывается правым кликом → «Открыть».
#
# Env для настоящей подписи (все опциональны, без них — ад-хок):
#   MACOS_SIGN_ID          "Developer ID Application: Имя (TEAMID)"
#   Нотаризация (любой из двух способов):
#     а) MACOS_NOTARY_PROFILE   имя профиля notarytool (xcrun notarytool store-credentials)
#     б) AC_API_KEY_ID + AC_API_ISSUER_ID + AC_API_KEY_PATH  (App Store Connect API-ключ .p8)
#
#   packaging/macos/build-app.sh [OUT_DIR]
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
BIN="$ROOT/target/release/bemyvpn-gui"
OUT="${1:-$ROOT/dist}"; mkdir -p "$OUT"; OUT="$(cd "$OUT" && pwd)"
VERSION="${VERSION:-1.0}"
[ -x "$BIN" ] || { echo "НЕТ бинаря $BIN — сначала: cargo build --release -p bmv-gui"; exit 1; }

WORK="$(mktemp -d)"; trap 'rm -rf "$WORK"' EXIT
APP="$WORK/BeMyVPN.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/bemyvpn-gui"
sed "s/__VERSION__/$VERSION/g" "$HERE/Info.plist" > "$APP/Contents/Info.plist"

# Иконка: полный iconset (все размеры + @2x) → iconutil (чёткая на всех масштабах).
ICON="$WORK/BeMyVPN.iconset"; mkdir -p "$ICON"
for s in 16 32 128 256 512; do
  sips -z "$s" "$s" "$ROOT/brand/icon-1024.png" --out "$ICON/icon_${s}x${s}.png" >/dev/null
  d=$((s * 2)); sips -z "$d" "$d" "$ROOT/brand/icon-1024.png" --out "$ICON/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICON" -o "$APP/Contents/Resources/bemyvpn.icns"

# ── подпись ──────────────────────────────────────────────────────────────────
if [ -n "${MACOS_SIGN_ID:-}" ]; then
  echo "подпись: Developer ID ($MACOS_SIGN_ID) + hardened runtime"
  # Сначала вложенный бинарь, потом весь бандл (изнутри наружу), с hardened runtime
  # и нашими entitlements — это требование нотаризации.
  codesign --force --timestamp --options runtime \
    --entitlements "$HERE/entitlements.plist" --sign "$MACOS_SIGN_ID" \
    "$APP/Contents/MacOS/bemyvpn-gui"
  codesign --force --timestamp --options runtime \
    --entitlements "$HERE/entitlements.plist" --sign "$MACOS_SIGN_ID" "$APP"
  codesign --verify --deep --strict --verbose=2 "$APP" && echo "подпись ок"
  SIGNED_REAL=1
else
  echo "подпись: АД-ХОК (сертификата нет — MACOS_SIGN_ID не задан)"
  codesign --force --deep --sign - "$APP"
  codesign --verify --deep --strict "$APP" && echo "подпись ок"
  SIGNED_REAL=0
fi

# DMG: один файл. Внутри — .app + ярлык на «Программы» (перетащил и готово).
# Архитектуру берём у машины: тот же скрипт собирает и для Apple Silicon, и для
# Intel-маков — их на руках ещё много.
DMG="$OUT/bemyvpn-macos-$([ "$(uname -m)" = arm64 ] && echo arm64 || echo x86_64).dmg"
DMGROOT="$WORK/dmgroot"; mkdir -p "$DMGROOT"
cp -R "$APP" "$DMGROOT/BeMyVPN.app"
ln -s /Applications "$DMGROOT/Applications"
hdiutil create -volname "BeMyVPN" -srcfolder "$DMGROOT" -ov -format UDZO "$DMG" >/dev/null

# ── нотаризация (только при настоящей подписи и заданных кредах) ──────────────
notarize() {
  [ "$SIGNED_REAL" = "1" ] || return 0
  local args=()
  if [ -n "${MACOS_NOTARY_PROFILE:-}" ]; then
    args=(--keychain-profile "$MACOS_NOTARY_PROFILE")
  elif [ -n "${AC_API_KEY_ID:-}" ] && [ -n "${AC_API_ISSUER_ID:-}" ] && [ -n "${AC_API_KEY_PATH:-}" ]; then
    args=(--key "$AC_API_KEY_PATH" --key-id "$AC_API_KEY_ID" --issuer "$AC_API_ISSUER_ID")
  else
    echo "нотаризация пропущена: креды не заданы (подпись есть, но без нотаризации Gatekeeper всё равно попросит правый клик)"
    return 0
  fi
  echo "нотаризация dmg…"
  xcrun notarytool submit "$DMG" "${args[@]}" --wait
  xcrun stapler staple "$DMG" && echo "нотаризация + staple ок → двойной клик без предупреждений"
}
notarize

echo "ГОТОВ: $DMG ($(du -h "$DMG" | cut -f1))"
