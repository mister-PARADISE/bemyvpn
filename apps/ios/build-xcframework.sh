#!/usr/bin/env bash
# Собрать ядро BeMyVPN (bmv-ffi) в BmvFFI.xcframework для iOS (устройство + симулятор).
# Требует АКТИВНЫЙ Xcode (не Command Line Tools):
#   sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
#   sudo xcodebuild -runFirstLaunch   # один раз, доустановит компоненты
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

rustup target add aarch64-apple-ios aarch64-apple-ios-sim aarch64-apple-ios-macabi >/dev/null 2>&1 || true

# Минимальная версия iOS. ОБЯЗАНА совпадать с IPHONEOS_DEPLOYMENT_TARGET в
# project.yml: без неё cargo и крейт `cc` (им собирается асм ring) берут версию
# из SDK — то есть свежайшую. Ядро тогда заявляет «нужен iOS 26.5» внутри
# приложения, обещающего iOS 16: линкер ругается, а на реальном устройстве с
# более старой iOS это уже не предупреждение, а поломка.
export IPHONEOS_DEPLOYMENT_TARGET=16.0

# LTO ОБЯЗАТЕЛЬНО выключен именно здесь. В профиле release стоит lto = true, и с
# ним rustc кладёт в статическую библиотеку LLVM-биткод вместо машинного кода.
# Дальше эту библиотеку читает линкер Xcode, а его LLVM старше нашего (rustc
# LLVM 22 против LLVM Apple) — биткод он не разбирает и просто не видит символы:
# сборка падает с «Undefined symbol: _bmv_connect» и остальными bmv_*.
# Переменная окружения перекрывает профиль только для этих трёх сборок, поэтому
# координатор, CLI и десктоп по-прежнему собираются с полным LTO.
export CARGO_PROFILE_RELEASE_LTO=off

echo "→ сборка под устройство (aarch64-apple-ios), min iOS $IPHONEOS_DEPLOYMENT_TARGET…"
cargo build --release -p bmv-ffi --target aarch64-apple-ios
echo "→ сборка под симулятор (aarch64-apple-ios-sim), min iOS $IPHONEOS_DEPLOYMENT_TARGET…"
cargo build --release -p bmv-ffi --target aarch64-apple-ios-sim
# Mac Catalyst собираем ОБЕ архитектуры и склеиваем в универсальную библиотеку.
# Mac App Store ожидает универсальный бинарь (Apple Silicon + Intel), и Xcode при
# архивации Catalyst тянет x86_64 тоже. Без Intel-среза архив падает с
# «Undefined symbols for architecture x86_64».
echo "→ сборка под Mac Catalyst (aarch64-apple-ios-macabi)…"
cargo build --release -p bmv-ffi --target aarch64-apple-ios-macabi
echo "→ сборка под Mac Catalyst Intel (x86_64-apple-ios-macabi)…"
cargo build --release -p bmv-ffi --target x86_64-apple-ios-macabi

MACABI="$ROOT/target/macabi-universal"
mkdir -p "$MACABI"
lipo -create \
  "$ROOT/target/aarch64-apple-ios-macabi/release/libbmv_ffi.a" \
  "$ROOT/target/x86_64-apple-ios-macabi/release/libbmv_ffi.a" \
  -output "$MACABI/libbmv_ffi.a"

HDR="$ROOT/apps/ios/headers"
rm -rf "$HDR"; mkdir -p "$HDR"
cp "$ROOT/apps/ios/bmv_ffi.h" "$HDR/"
cat > "$HDR/module.modulemap" <<'MM'
module BmvFFI {
    header "bmv_ffi.h"
    export *
}
MM

OUT="$ROOT/apps/ios/BmvFFI.xcframework"
rm -rf "$OUT"
xcodebuild -create-xcframework \
  -library "$ROOT/target/aarch64-apple-ios/release/libbmv_ffi.a"        -headers "$HDR" \
  -library "$ROOT/target/aarch64-apple-ios-sim/release/libbmv_ffi.a"    -headers "$HDR" \
  -library "$MACABI/libbmv_ffi.a"                                       -headers "$HDR" \
  -output "$OUT"

echo "✅ готово: $OUT"
