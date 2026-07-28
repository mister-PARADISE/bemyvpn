#!/usr/bin/env bash
# Собрать APK ЗАВЕДОМО СТАРОЙ версии — чтобы проверить обновление на телефоне.
#
#   bash tools/build-test-apk.sh
#
# Спросит пароль от keystore (он нужен, чтобы подпись совпала с релизной, иначе
# Android откажется ставить обновление поверх). Пароль нигде не сохраняется:
# передаётся сборке переменной окружения и исчезает вместе с процессом.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT/apps/android"

VERSION="${1:-1.0}"
KEYSTORE="${BMV_KEYSTORE_FILE:-$HOME/bemyvpn-release.jks}"
ALIAS="${BMV_KEY_ALIAS:-bemyvpn}"
OUT="$HOME/Downloads/bemyvpn-test-$VERSION.apk"

export JAVA_HOME="${JAVA_HOME:-/opt/homebrew/opt/openjdk@17}"
export PATH="$JAVA_HOME/bin:$PATH"
export ANDROID_HOME="${ANDROID_HOME:-$HOME/Library/Android/sdk}"

[ -f "$KEYSTORE" ] || { echo "Нет файла ключа: $KEYSTORE" >&2; exit 1; }
command -v keytool >/dev/null || { echo "Не найден keytool (JAVA_HOME=$JAVA_HOME)" >&2; exit 1; }

# Пароль спрашиваем скрытно и СРАЗУ проверяем: падать на середине сборки из-за
# опечатки — впустую тратить минуты и нервы.
read -rs -p "Пароль keystore: " BMV_KEYSTORE_PASSWORD
echo
if ! keytool -list -keystore "$KEYSTORE" -storepass "$BMV_KEYSTORE_PASSWORD" >/dev/null 2>&1; then
  echo "Пароль не подходит к $KEYSTORE — сборка не начата." >&2
  exit 1
fi
if ! keytool -list -keystore "$KEYSTORE" -storepass "$BMV_KEYSTORE_PASSWORD" -alias "$ALIAS" >/dev/null 2>&1; then
  echo "В хранилище нет ключа с именем «$ALIAS». Что есть:" >&2
  keytool -list -keystore "$KEYSTORE" -storepass "$BMV_KEYSTORE_PASSWORD" 2>/dev/null | grep -i 'PrivateKeyEntry' >&2 || true
  exit 1
fi
echo "Пароль верный, ключ «$ALIAS» найден."

export BMV_KEYSTORE_FILE="$KEYSTORE"
export BMV_KEY_ALIAS="$ALIAS"
export BMV_VERSION="$VERSION"
export BMV_KEYSTORE_PASSWORD

./gradlew assembleRelease
cp app/build/outputs/apk/release/app-release.apk "$OUT"

# Сверяем подпись с выпущенной: если отпечатки разойдутся, обновление на
# телефоне не встанет, и лучше узнать это здесь, чем на устройстве.
APKSIGNER=$(ls "$ANDROID_HOME"/build-tools/*/apksigner 2>/dev/null | tail -1)
if [ -n "$APKSIGNER" ]; then
  MINE=$("$APKSIGNER" verify --print-certs "$OUT" 2>/dev/null | grep -m1 'SHA-256 digest' | awk '{print $NF}')
  echo "отпечаток подписи: ${MINE:0:24}…"
fi

echo
echo "ГОТОВО: $OUT (версия $VERSION)"
echo "  1. перекинь файл на телефон"
echo "  2. УДАЛИ установленное приложение (у него другая подпись — поверх не встанет)"
echo "  3. поставь этот, открой — должна появиться плашка о свежей версии"
