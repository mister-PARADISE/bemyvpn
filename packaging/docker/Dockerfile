# BeMyVPN — хост в контейнере.
#
# ЗАЧЕМ ЭТО ЕСТЬ. Самый доказанный способ набрать добровольцев — попасть внутрь
# того, что у человека УЖЕ работает круглосуточно. У Tor Snowflake примерно треть
# всего парка прокси пришла из одной галочки в Orbot, а не из отдельного
# приложения. Нашего браузерного варианта быть не может, поэтому наш аналог —
# homelab: у этих людей машина и так не выключается, а Docker для них родной
# способ поставить что угодно.
#
# Берём готовый файл выпуска, а не собираем из исходников: ровно то же самое
# делает `install.sh`, файл тот же и приходит по HTTPS с github.com. Кто хочет
# собрать сам — `cargo build --release -p bmv-cli`, образ тогда не нужен.

ARG UBUNTU=24.04

# ── Слой загрузки ────────────────────────────────────────────────────────────
FROM ubuntu:${UBUNTU} AS fetch
ARG TARGETARCH
ARG VERSION=latest
# Ожидаемая sha256 файла (необязательно). Задали — проверим и не пустим подмену:
#   docker build --build-arg SHA256=03d59f… .
ARG SHA256=

RUN apt-get update \
 && apt-get install -y --no-install-recommends curl ca-certificates \
 && rm -rf /var/lib/apt/lists/*

RUN set -eu; \
    case "$TARGETARCH" in \
      amd64) asset=bemyvpn-linux-x86_64-terminal ;; \
      arm64) asset=bemyvpn-linux-arm64-terminal ;; \
      *) echo "неподдерживаемая архитектура: $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    if [ "$VERSION" = "latest" ]; then \
      url="https://github.com/mister-PARADISE/bemyvpn/releases/latest/download/$asset"; \
    else \
      url="https://github.com/mister-PARADISE/bemyvpn/releases/download/$VERSION/$asset"; \
    fi; \
    echo "качаю $url"; \
    curl -fL --retry 3 --connect-timeout 20 -o /bemyvpn "$url"; \
    # Пустой или крошечный файл = скачалась страница ошибки, а не программа.
    # Тот же сторож стоит в install.sh, и по той же причине.
    size=$(wc -c < /bemyvpn); \
    [ "$size" -gt 1000000 ] || { echo "скачался файл на $size байт — это не программа" >&2; exit 1; }; \
    if [ -n "$SHA256" ]; then \
      echo "$SHA256  /bemyvpn" | sha256sum -c - ; \
    fi; \
    chmod 755 /bemyvpn; \
    # Проверяем ЗДЕСЬ, а не при запуске: образ, который не запускается, не должен
    # собраться вовсе. Заодно ловится несовпадение glibc базового образа с той,
    # под которую собран выпуск (см. issue #4).
    /bemyvpn version

# ── Итоговый образ ───────────────────────────────────────────────────────────
FROM ubuntu:${UBUNTU}

# ca-certificates нужны: клиент ходит к координатору по HTTPS/WSS.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --create-home --home-dir /config --shell /usr/sbin/nologin bemyvpn

COPY --from=fetch /bemyvpn /usr/local/bin/bemyvpn

# КОНФИГ ОБЯЗАН ПЕРЕЖИВАТЬ ПЕРЕЗАПУСК. В нём лежат `id` хоста и `token` владения
# записью в каталоге; пустой `id` означает НОВУЮ СЛУЧАЙНУЮ ЛИЧНОСТЬ на сессию.
# Без тома контейнер после каждого рестарта выходил бы в каталог новым хостом:
# прежний код сети переставал бы работать у тех, кому его уже раздали.
ENV BEMYVPN_CONFIG=/config/config.toml
VOLUME /config

# РАЗДАЧА НЕ ТРЕБУЕТ ROOT — это свойство самого продукта (хост работает через
# userspace-стек и обычные сокеты, без TUN и без правки маршрутов), и здесь оно
# просто соблюдается. Гостём из этого образа подключиться НЕЛЬЗЯ и не нужно:
# для гостя требуется TUN, то есть привилегии, которых мы сознательно не даём.
USER bemyvpn
WORKDIR /config

# Живость меряем тем же, чем её меряет человек: координатор отвечает?
HEALTHCHECK --interval=60s --timeout=15s --start-period=20s --retries=3 \
  CMD ["/usr/local/bin/bemyvpn", "ping"]

ENTRYPOINT ["/usr/local/bin/bemyvpn"]
# --tunnel = раздавать по-настоящему. Без --autostart: службу здесь заводить
# нечему и незачем, перезапуском занимается сам Docker (restart: unless-stopped).
CMD ["host", "--tunnel"]
