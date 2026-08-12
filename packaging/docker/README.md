# BeMyVPN в Docker — стать выходной точкой за одну команду

*(English below.)*

Если у вас уже крутится домашний сервер, NAS или VPS с Docker — это самый
короткий способ отдать простаивающий канал тем, у кого доступа к половине
интернета просто нет.

```bash
git clone https://github.com/mister-PARADISE/bemyvpn
cd bemyvpn/packaging/docker
docker compose up -d
```

Всё. Ваша машина в каталоге, к вам могут подключаться.

```bash
docker compose logs -f     # что происходит, и ваш код сети
docker compose down        # остановить
```

## Что нужно знать

**Права root не нужны.** Процесс внутри контейнера работает не от root, и это не
натяжка ради галочки: хост в BeMyVPN принципиально не поднимает TUN и не правит
маршруты — он разбирает пакеты гостя userspace-стеком и открывает обычные сокеты.

**`network_mode: host` — обязателен, и это не лень.** Связь строится пробитием
NAT: обе стороны узнают свой внешний адрес и бьют навстречу с того же сокета.
Мост Docker ставит поверх вашего NAT ещё один — наружу уйдёт подменённый порт, и
координатор назовёт гостю адрес, по которому вас нет. Пробросить порты вручную
нельзя: порт заранее неизвестен, его выбирает система.

Дырой это не является: контейнер не открывает входящих портов (соединение к
координатору исходящее), внутренние адреса гостю закрыты самим приложением в обе
стороны, процесс не привилегированный.

**Том с конфигом терять нельзя.** В нём лежат идентификатор хоста и токен
владения записью в каталоге. Удалите том — машина выйдет в каталог **новым**
хостом, и код сети, который вы раздали друзьям, перестанет работать.

**Гостём отсюда подключиться нельзя** — и не нужно. Гостю требуется TUN, то есть
привилегии, которых образ сознательно не даёт. Для подключения берите обычное
приложение.

## Настройки

Правится `command` в `docker-compose.yml`:

| Флаг | Зачем |
|---|---|
| `--name "…"` | своё имя в каталоге вместо имени машины |
| `--max 16` | свой лимит гостей вместо подобранного по памяти |
| `--hidden` | не показываться в общем списке — только по коду |
| `--password …` | пароль на вход; тогда сеть всегда скрытая |

Помогать только своим: поставьте `--hidden`, возьмите код из
`docker compose logs` и передайте тем, кому доверяете.

## Прежде чем включить — честно

Ваш IP становится видимым источником трафика гостя. Это то же самое, что у
выходного узла Tor: возможны капчи на сайтах, а теоретически и письмо от
провайдера. Мы пишем это здесь, а не мелким шрифтом в FAQ.

Что при этом **не** происходит: денег никто никому не платит и не берёт — ни вам,
ни с вас, и в коде негде им завестись; ваш канал никому не перепродаётся; чтобы
пользоваться VPN, раздавать не обязательно. Раздача включается только тем, что вы
её включили.

## Своя версия и проверка файла

```bash
# конкретный выпуск вместо свежего
docker compose build --build-arg VERSION=v1.46

# с проверкой хэша (хэши публикуются в описании каждого релиза)
docker build --build-arg VERSION=v1.46 --build-arg SHA256=03d59f… .
```

Образ берёт готовый файл выпуска — ровно то же делает `install.sh`. Кто хочет
собрать из исходников: `cargo build --release -p bmv-cli`, образ тогда не нужен.

---

# BeMyVPN in Docker — become an exit node in one command

If you already run a home server, NAS or VPS with Docker, this is the shortest way
to donate idle bandwidth to people who have no access to half the internet.

```bash
git clone https://github.com/mister-PARADISE/bemyvpn
cd bemyvpn/packaging/docker
docker compose up -d
```

That's it — your machine is in the directory and people can connect.

**No root needed.** The process runs unprivileged. This isn't a technicality: a
BeMyVPN host never creates a TUN device or touches routing. It parses the guest's
IP packets in a userspace TCP/IP stack and opens ordinary outbound sockets.

**`network_mode: host` is required.** Connections are established by NAT
hole-punching — both sides learn their external address and punch toward each
other from the same socket. Docker's bridge adds a second NAT on top of yours, so
a rewritten port goes out and the coordinator hands the guest an address where you
aren't. Manual port mapping can't help: the port isn't known in advance.

This is not a security hole — the container opens no inbound ports (the
coordinator connection is outbound), internal addresses are denied to guests in
both directions by the application itself, and the process is unprivileged.

**Don't lose the config volume.** It holds the host id and the ownership token for
the directory entry. Delete it and your machine rejoins as a *new* host — the
network code you gave your friends stops working.

**You cannot connect as a guest from this image**, by design: a guest needs TUN,
which means privileges this image deliberately withholds. Use the regular app.

Options are set via `command` in `docker-compose.yml`: `--name`, `--max`,
`--hidden` (unlisted, reachable by code only), `--password`.

**Before you switch it on, plainly:** your IP becomes the visible source of the
guest's traffic — the same situation as a Tor exit relay, so expect occasional
CAPTCHAs and, in principle, a letter from your ISP. What does *not* happen: nobody
pays you and nobody charges you (there is no payment path anywhere in the code),
your bandwidth is never resold, and using the VPN never requires sharing. Sharing
happens only because you turned it on.
