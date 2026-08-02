#!/bin/sh
# Установка BeMyVPN (терминальная версия) одной командой:
#
#   curl -fsSL https://raw.githubusercontent.com/mister-PARADISE/bemyvpn/main/install.sh | sh
#
# Своя папка (без sudo):  BMV_INSTALL_DIR=~/bin  … | sh
#
# Скачивает свежий релиз под вашу систему, кладёт в PATH и проверяет, что
# запускается. Ничего не собирает, ничего не требует, кроме curl.
#
# Специально на /bin/sh, а не на bash: на голом сервере (Debian, Alpine,
# контейнер) bash бывает не установлен, а sh есть всегда.
set -eu

REPO="mister-PARADISE/bemyvpn"
NAME="bemyvpn"

say() { printf '%s\n' "$*"; }
die() { printf 'Ошибка: %s\n' "$*" >&2; exit 1; }

# ── какой файл релиза нам нужен ───────────────────────────────────────────────
os=$(uname -s)
arch=$(uname -m)
case "${os}:${arch}" in
  Linux:x86_64)          asset="bemyvpn-linux-x86_64-terminal" ;;
  Darwin:arm64)          asset="bemyvpn-macos-arm64-terminal" ;;
  Darwin:x86_64)         die "маки на Intel не поддерживаются — релизы только под Apple Silicon" ;;
  Linux:aarch64|Linux:arm64) asset="bemyvpn-linux-arm64-terminal" ;;
  *) die "неизвестная система ${os} ${arch}" ;;
esac

command -v curl >/dev/null 2>&1 || die "нужен curl (apt install curl / apk add curl)"

# ── куда ставить ──────────────────────────────────────────────────────────────
# Проверяем право записи, а не «я root»: на маке /usr/local/bin обычно и так наш.
if [ -n "${BMV_INSTALL_DIR:-}" ]; then
  dir="${BMV_INSTALL_DIR}"
  sudo=""
  mkdir -p "${dir}"
elif [ -d /usr/local/bin ] && [ -w /usr/local/bin ]; then
  dir=/usr/local/bin
  sudo=""
elif command -v sudo >/dev/null 2>&1; then
  dir=/usr/local/bin
  sudo="sudo"
  say "Для установки в ${dir} потребуется пароль sudo."
else
  dir="${HOME}/.local/bin"
  sudo=""
  mkdir -p "${dir}"
fi

url="https://github.com/${REPO}/releases/latest/download/${asset}"
tmp=$(mktemp) || die "не создать временный файл"
trap 'rm -f "${tmp}"' EXIT INT TERM

say "Скачиваю ${asset}…"
curl -fL --progress-bar --show-error --retry 3 --connect-timeout 20 -o "${tmp}" "${url}" \
  || die "не удалось скачать. Если GitHub недоступен — подключитесь к VPN и повторите"

# Пустой или крошечный файл = скачалась страница ошибки, а не программа.
size=$(wc -c < "${tmp}" | tr -d ' ')
[ "${size}" -gt 1000000 ] || die "скачался файл на ${size} байт — это не программа"

chmod +x "${tmp}"
# Проверяем ДО установки: подсунуть в PATH нерабочий файл хуже, чем не поставить.
ver=$("${tmp}" version 2>/dev/null) || die "скачанный файл не запускается на этой системе"

# Ставим через временное имя рядом с целью + mv: на месте назначения никогда не
# окажется наполовину записанного файла, и переписать работающий тоже можно.
${sudo} mv "${tmp}" "${dir}/.${NAME}.new" || die "нет прав на запись в ${dir}"
trap - EXIT INT TERM
${sudo} chmod 755 "${dir}/.${NAME}.new"
${sudo} mv "${dir}/.${NAME}.new" "${dir}/${NAME}" \
  || { ${sudo} rm -f "${dir}/.${NAME}.new"; die "не удалось установить в ${dir}"; }

say ""
say "Готово: ${dir}/${NAME}  (версия ${ver})"

case ":${PATH}:" in
  *":${dir}:"*) say "Команда доступна: ${NAME}" ;;
  *)
    say "ВНИМАНИЕ: ${dir} не в PATH — команда ${NAME} пока не найдётся."
    say "  добавьте в ~/.profile:  export PATH=\"${dir}:\$PATH\""
    ;;
esac

say ""
say "Дальше:"
say "  ${NAME}          — меню (настройка, подключение, раздача)"
say "  ${NAME} host     — раздавать свой интернет"
say "  ${NAME} server   — поднять свой координатор"
say "  ${NAME} update   — обновиться до свежего релиза"
