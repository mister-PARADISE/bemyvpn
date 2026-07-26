#!/usr/bin/env bash
# Сборка Android APK одной командой.
#
# ЗАЧЕМ СКРИПТ: cargo-ndk выставляет свой RUSTFLAGS, и он ПЕРЕБИВАЕТ
# .cargo/config.toml — поэтому remap путей приходится передавать здесь явно.
# Без него в libbmv_android.so попадает $HOME/.cargo/... и любой, кто скачал
# APK, читает имя пользователя сборочной машины через `strings`.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT/apps/android"

export ANDROID_HOME="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
export ANDROID_NDK_HOME="${ANDROID_NDK_HOME:-$(ls -d "$ANDROID_HOME"/ndk/* | tail -1)}"
export JAVA_HOME="${JAVA_HOME:-$(/usr/libexec/java_home -v 17 2>/dev/null || echo /opt/homebrew/opt/openjdk@17)}"
export PATH="$JAVA_HOME/bin:$PATH"

# Нативная библиотека (Rust) → jniLibs. Пути сборочной машины вырезаем.
( cd rust && RUSTFLAGS="--remap-path-prefix=$HOME=/build" \
    cargo ndk -t arm64-v8a -o ../app/src/main/jniLibs build --release )

./gradlew assembleRelease

APK="app/build/outputs/apk/release/app-release.apk"
LEAK=$(strings "$APK" | grep -ci "$(basename "$HOME")" || true)
if [ "$LEAK" != "0" ]; then
  echo "ОШИБКА: в APK попало имя пользователя ($LEAK совпадений) — не выкладывать!" >&2
  exit 1
fi
echo "ГОТОВ: $ROOT/apps/android/$APK (утечек путей нет)"
