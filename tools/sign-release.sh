#!/usr/bin/env bash
# Собрать манифест обновления для выпущенного релиза и ПОДПИСАТЬ его.
#
# Запускать ПОСЛЕ того, как CI собрал релиз и выложил файлы.
#
#   bash tools/sign-release.sh v1.6
#
# Приватный ключ (по умолчанию ~/bemyvpn-update-key.pem) НИКОГДА не попадает ни
# в репозиторий, ни в CI: подпись ставится вручную и офлайн. Утечка ключа = чужой
# код у всех пользователей, поэтому автоматизировать этот шаг НЕЛЬЗЯ — он должен
# оставаться осознанным действием человека.
set -euo pipefail

TAG="${1:?укажите тег релиза, например v1.6}"
KEY="${BMV_UPDATE_KEY:-$HOME/bemyvpn-update-key.pem}"
REPO="${BMV_REPO:-mister-PARADISE/bemyvpn}"
VERSION="${TAG#v}"

[ -f "$KEY" ] || { echo "нет приватного ключа: $KEY" >&2; exit 1; }

WORK="$(mktemp -d)"; trap 'rm -rf "$WORK"' EXIT
echo "Скачиваю файлы релиза $TAG…"
gh release download "$TAG" -R "$REPO" -D "$WORK/files"

# sha256 каждого файла. Манифест собираем СТРОГО детерминированно (ключи
# отсортированы): подпись считается по точным байтам, и пересборка манифеста
# другим порядком сделала бы её недействительной.
FILES_JSON=$(
  cd "$WORK/files"
  for f in $(ls | sort); do
    printf '%s\t%s\n' "$f" "$(shasum -a 256 "$f" | cut -d' ' -f1)"
  done | python3 -c '
import sys, json
d = dict(line.rstrip("\n").split("\t") for line in sys.stdin if line.strip())
print(json.dumps(d, sort_keys=True, separators=(",", ":"), ensure_ascii=False))'
)

# min_supported — ниже какой версии клиент больше не совместим с сетью.
# По умолчанию равен текущей минус ничего: совместимость не ломаем. Задать
# осознанно можно переменной, когда протокол реально изменится.
MIN="${BMV_MIN_SUPPORTED:-1.0}"

python3 - "$VERSION" "$MIN" "$REPO" "$TAG" "$FILES_JSON" > "$WORK/manifest.json" <<'PY'
import json, sys
version, minimum, repo, tag, files = sys.argv[1:6]
print(json.dumps({
    "version": version,
    "min_supported": minimum,
    "notes": f"https://github.com/{repo}/releases/tag/{tag}",
    "files": json.loads(files),
}, sort_keys=True, separators=(",", ":"), ensure_ascii=False), end="")
PY

# Подпись Ed25519 по ТОЧНЫМ байтам манифеста.
openssl pkeyutl -sign -inkey "$KEY" -rawin -in "$WORK/manifest.json" -out "$WORK/manifest.sig.bin"
xxd -p -c 256 "$WORK/manifest.sig.bin" | tr -d '\n' > "$WORK/manifest.sig"

echo "Манифест:"; cat "$WORK/manifest.json"; echo
echo "Подпись: $(cat "$WORK/manifest.sig")"

# Самопроверка перед публикацией: подпись обязана сходиться с ключом, вшитым в
# приложение. Иначе выложим релиз, который никто не сможет установить.
PUB=$(openssl pkey -in "$KEY" -pubout -outform DER | tail -c 32 | xxd -p -c 32)
EMBEDDED=$(grep -o 'UPDATE_PUBKEY_HEX: &str = "[0-9a-f]*"' crates/bmv-common/src/update.rs | grep -o '[0-9a-f]\{64\}')
if [ "$PUB" != "$EMBEDDED" ]; then
  echo "ОШИБКА: ключ не тот, что вшит в приложение." >&2
  echo "  в приложении: $EMBEDDED" >&2
  echo "  у этого ключа: $PUB" >&2
  exit 1
fi
echo "✓ ключ совпадает с вшитым в приложение"

gh release upload "$TAG" "$WORK/manifest.json" "$WORK/manifest.sig" -R "$REPO" --clobber
echo "✓ манифест и подпись выложены в релиз $TAG"
