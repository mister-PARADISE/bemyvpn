# Короткие форматы: посты, анонсы, комментарии

Всё ниже написано в формате **«я автор, вот открытый код»**. Это не вежливость,
а расчёт: на Хабре, Реддите и HN накрутку вычисляют быстро и наказывают жёстко —
теневым баном домена, минусами в карму, «этот проект спамил, не ставьте». Один
пойманный фейковый комментарий стоит дороже, чем сто честных постов.

Правило для всех площадок: **не постить одинаковый текст в два места.** Кросспост
одного и того же абзаца — первый признак спама и для людей, и для антиспама.

---

## 1. Телеграм — анонс в свой канал / чат

> Я сделал VPN, у которого нет VPN-сервера.
>
> Выходной точкой становится компьютер человека, который решил помочь: чей-то VPS,
> домашний сервер, старый ноутбук. Тот, кто делится, жмёт одну кнопку — и ему **не
> нужны права администратора**, вообще нигде. Тот, кому нужен доступ, жмёт другую.
>
> Между ними — маленький сервер-доска-объявлений, который знакомит двоих и уходит.
> Трафик идёт напрямую, мимо него. Он его не видит физически, а не «обещает не
> смотреть».
>
> Без регистрации, без оплаты, открытый код, Apache-2.0. Rust, четыре платформы.
>
> Сейчас нужны не звёзды, а **хосты**: в каталоге бывает три штуки, и человек,
> которому нужен доступ, видит пустой список и уходит. Если есть простаивающий
> VPS — это две строки:
>
> `curl -fsSL .../install.sh | sh`
> `sudo bemyvpn host --tunnel --autostart`
>
> github.com/mister-PARADISE/bemyvpn

---

## 2. Телеграм — короткий вариант для чужих чатов (где разрешено)

**Сначала спросите админа.** Без спроса — это спам, и вас справедливо забанят.

> Привет. Я автор опенсорсного P2P-VPN: выходная точка — не сервер сервиса, а
> компьютер человека, который решил помочь. Хостом можно стать без прав
> администратора и без настройки роутера — в этом весь фокус, ради него переписан
> сетевой слой.
>
> Ищу людей с простаивающими VPS, готовых раздать. Код открыт, Apache-2.0:
> github.com/mister-PARADISE/bemyvpn
>
> Если формат не для этого чата — снесу, скажите.

---

## 3. Reddit — r/rust (английский, техническая подача)

**Заголовок:**
`Adding TCP window scaling, congestion control and backpressure to a userspace TCP/IP stack — six fixes that only work together`

**Текст:**

> I maintain a P2P VPN written in Rust where the exit node is a regular person's
> machine. The key design constraint: **hosting requires no root/admin privileges
> on any platform.** Instead of a TUN interface and iptables, the host parses the
> guest's IP packets with a userspace TCP/IP stack (`ipstack`) and opens ordinary
> sockets outbound.
>
> That made TCP our problem. Throughput was capped in the single-digit Mbps and
> scaled with RTT, not bandwidth — the classic `window / RTT` ceiling, because the
> upstream stack had no RFC 7323 window scaling (16-bit window field = 64 KiB cap).
>
> Adding window scaling alone made things **worse**: the stack sent the entire
> receive window at once, which with a 1 MiB window is a megabyte burst into
> whatever buffers are on the path. There was no congestion control at all.
>
> Six fixes in total, and the interesting part is that several of them make things
> worse in isolation:
>
> 1. RFC 7323 window scaling
> 2. Congestion window — slow start + AIMD (didn't exist upstream)
> 3. Receive window that actually reaches zero — it had `avail.max(mtu)`, so it
>    **never closed** and there was no receiver backpressure whatsoever
> 4. dup-ACK detection by cumulative ACK instead of window equality — the old
>    condition silently disabled fast retransmit on real traffic, so every loss
>    waited a full RTO
> 5. Fast-recovery guard — one congestion signal per loss event, not per dup-ACK
>    (a full window of dup-ACKs was halving cwnd hundreds of times)
> 6. Adaptive RTO per RFC 6298 with Karn's algorithm
>
> All fork changes are tagged `BeMyVPN fork:` in `vendor/ipstack/`, each with tests.
>
> Repo (Apache-2.0): github.com/mister-PARADISE/bemyvpn
>
> Fair warning before you click: **the app's UI is currently Russian-only** —
> strings are inline in the code, extracting them is on the list. The networking
> code and comments are what I'm actually showing here.
>
> Happy to be told I got the congestion control wrong — I wrote it from RFCs and
> textbooks, and `vendor/ipstack/src/stream/tcb.rs` is exactly where an experienced
> pair of eyes is worth the most.

---

## 4. Hacker News — Show HN

**Заголовок:** `Show HN: A VPN where the exit node is a volunteer's computer, no root required`

**Первый комментарий от себя (обязательно — на HN так принято):**

> Author here. The design constraint that shaped everything: **hosting must require
> no admin privileges.** Nobody is going to type a sudo password and edit firewall
> rules to do a stranger a favor.
>
> So the host never creates a TUN interface or touches the kernel. It receives the
> guest's IP packets, terminates TCP/UDP in a userspace stack, and opens ordinary
> outbound sockets — like any browser. Root is needed only on the *guest* side,
> because every OS requires it for a TUN device. The asymmetry is the point: the
> person helping does nothing, the person who needs help confirms one system dialog.
>
> The coordinator is a directory and signalling server on a single WebSocket. It
> is **not** in the data path — after NAT hole-punching the tunnel is direct, so
> there is nothing to log or subpoena. You can self-host one with a single binary
> (Let's Encrypt is built in, no certbot, no nginx).
>
> Things it deliberately doesn't have, since I'd rather you hear it from me:
> no relay (symmetric NAT on both ends = no connection, and it says so honestly
> instead of spinning), no kill switch (deleted the config knob rather than fake
> it), IPv6 is blocked rather than tunnelled (otherwise v6 leaks around the tunnel
> while the UI says "Protected"), no code signing, and **the UI is Russian-only**
> right now.
>
> Crypto is Noise XX / ChaCha20-Poly1305 via `snow` — I wrote none of it myself.
> The obfuscated mode adds Elligator2 for the ephemeral key (via Tor's
> `curve25519-elligator2` crate), per-session padding with jitter inside the AEAD,
> and QUIC-style header protection on the nonce so there's no monotonic counter on
> the wire.
>
> Apache-2.0, Rust, ~27k lines of our own code. Would especially value review of
> `crates/bmv-protocol/src/noise.rs`.

---

## 5. Комментарий под чужим постом (RU) — когда он реально уместен

Уместен, если в треде **уже** обсуждают: обход блокировок, самохостинг VPN,
недоверие к VPN-сервисам, куда девать простаивающий VPS. В остальных случаях —
не уместен, и лучше промолчать.

Всегда с раскрытием авторства. Всегда одной репликой, без «а вот ещё» вторым
комментарием.

**Вариант А — в треде про недоверие к VPN-сервисам:**

> Тут корень в том, что выходная точка принадлежит сервису, и проверить его
> no-logs-обещание нельзя в принципе. Я к этому подошёл с другой стороны и делаю
> опенсорсную штуку, где выходная точка — компьютер конкретного человека, а
> координирующий сервер не в пути трафика вообще (соединение прямое, после
> пробития NAT). Ему нечего логировать физически.
>
> Раскрываю карты: я автор, так что читайте с поправкой. Код открыт, ругать можно
> предметно: github.com/mister-PARADISE/bemyvpn

**Вариант Б — в треде «куда девать простаивающий VPS»:**

> Как вариант — отдать его трафик тем, у кого нет доступа к половине интернета.
> Я автор опенсорсного P2P-VPN, где выходной точкой становится чей-то сервер;
> ставится в две строки и переживает перезагрузку. Прав администратора для
> раздачи не нужно (host работает через userspace TCP/IP-стек и обычные сокеты,
> без TUN и iptables) — но на VPS `sudo` всё же нужен, чтобы прописалась служба
> systemd.
>
> Apache-2.0: github.com/mister-PARADISE/bemyvpn

**Вариант В — в треде про NAT / hole punching / сетевые кишки:**

> У меня ровно этот сценарий в проекте: hole punching без relay, STUN-пул из
> публичных серверов плюс свой координатор. Работает у большинства, но за
> симметричным NAT с обеих сторон — нет, и я предпочёл честно писать об этом в
> README, а не изображать «сейчас подключимся».
>
> Отдельная засада была в том, что во время фазы пробивания в сокет прилетают
> поздние ответы STUN и хвосты от других кандидатов — если принять их за данные
> пира, соединение встаёт не туда. Лечится приёмом только от адресов-кандидатов.
>
> Я автор, код открыт: github.com/mister-PARADISE/bemyvpn

---

## 6. GitHub — описание репозитория и топики

**Description (одна строка, видна в поиске):**

> P2P VPN where the exit node is a volunteer's machine. Hosting needs no root —
> userspace TCP/IP stack, no TUN, no iptables. Rust, 4 platforms.

**Topics:** `vpn` `p2p` `rust` `censorship-circumvention` `nat-traversal`
`hole-punching` `noise-protocol` `wireguard` `privacy` `android` `ios` `self-hosted`

Топики — недооценённый канал: по ним репозиторий находят через поиск GitHub без
всяких статей.

---

## 7. Ответ на неизбежные вопросы

Их зададут в комментариях под каждой статьёй. Ответы лучше заготовить.

**«А меня не посадят за трафик гостя?»**
> Правовой оценки я вам дать не могу и не буду делать вид, что могу — юрисдикции
> разные. Что могу сказать технически: с точки зрения сети исходящие соединения
> идут с вашего IP, ровно как у любой выходной ноды. Это тот же вопрос, который
> задают про выходные узлы Tor, и решать его каждому за себя. Если сомневаетесь —
> раздавайте «скрыто», по коду, только тем, кого знаете лично. Такой режим есть
> ровно для этого.

**«Чем это лучше WireGuard/Amnezia/своего сервера?»**
> Ничем, если у вас уже есть свой сервер и вы умеете его настраивать — тогда
> берите WireGuard, честно. Эта штука для другого случая: когда сервера нет и
> настраивать вы ничего не будете, а помочь готов кто-то незнакомый. И для
> обратного случая — когда сервер есть, а желание помочь некуда приложить.

**«Опенсорс — а бинарники вы собираете сами, кто их проверял?»**
> Справедливо. Сборка идёт в GitHub Actions из публичного репозитория, workflow
> лежит рядом с кодом. Отдельной подписи нет — она защищала бы только от кражи
> самого аккаунта GitHub, а стоила бы офлайн-ключа и ручной подписи каждого
> релиза. Кто не доверяет — `cargo build --release -p bmv-cli`, Rust 1.85+.

**«Координатор видит, кто с кем соединился — это же деанон.»**
> Да, это честное ограничение, и оно записано в архитектуре. Координатор знает
> факт знакомства и IP-адреса сторон в момент знакомства. Он не видит трафик,
> но метаданные знакомства у него есть. Кому это критично — свой координатор
> поднимается одной командой, а список координаторов в конфиге это массив, не
> константа.

**«Скорость?»**
> Упирается в канал хоста. Про то, как мы вытаскивали userspace-TCP из
> шестимегабитного потолка, у меня отдельная статья — там window scaling,
> congestion control и ещё четыре вещи, которые не работают по отдельности.

**«Почему интерфейс только русский?»**
> Строки лежат в коде, а не в файлах локализации. Это технический долг, я его
> признаю, и это сейчас главный ограничитель проекта. Вынести строки — задача
> понятная, руки не дошли. PR приму с радостью.
