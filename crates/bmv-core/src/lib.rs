//! bmv-core — ОРКЕСТРАТОР и единственный публичный фасад для всех оболочек.
//!
//! Kotlin (Android), Swift (Apple), CLI, GUI — все зовут только `BmvEngine`.
//! Логики VPN в оболочках нет: она вся здесь и в слоях, которые core связывает.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use parking_lot::Mutex;
use std::time::Duration;

use bmv_common::{Link, Result};
use bmv_config::Config;
use bmv_net::UdpEndpoint;
use bmv_protocol::{Protocol, Registry};

/// Маркер auth-кадра гостя (первый пакет при запароленном хосте). Настоящий
/// IP-пакет так не начинается (версия 4/6 в старшем ниббле), keepalive=0, BYE=1.
const AUTH_MARKER: u8 = 0xF0;

/// Движок BeMyVPN. Держит конфиг и реестр протоколов, связывает слои.
/// Дёшево клонируется (общее состояние за Arc) — оболочки делят его между задачами.
#[derive(Clone)]
pub struct BmvEngine {
    config: Config,
    /// Список STUN-серверов, РАЗОБРАННЫЙ ОДИН РАЗ — при сборке движка.
    ///
    /// `StunConfig::resolve` читает файл с диска, а звали её из пяти мест, одно
    /// из которых — тик хоста: то есть блокирующее чтение файла раз в десять
    /// секунд из async-задачи, всё время раздачи. На телефонах файла нет вовсе
    /// (конфига там нет как явления), и это был отказ open() каждые десять
    /// секунд навсегда. Настройка обязана доезжать до места решения РАЗОБРАННОЙ,
    /// а не выводиться заново из файловой системы на каждый пинг.
    ///
    /// Пустой список — законное значение: он означает «взять встроенный пул»
    /// (см. `StunConfig::resolve` и `bmv_net`).
    stun: Arc<[String]>,
    protocols: Arc<Registry>,
    /// ЕДИНСТВЕННЫЙ клиент координатора на движок (персистентный WebSocket:
    /// сокет = живость хоста, каталог пушем). Создаётся лениво при первом
    /// обращении и переиспользуется ВСЕМИ вызовами: без этого каждый
    /// health/watch/heartbeat открывал бы свой сокет (утечки + мигание статуса,
    /// а хост «умирал» бы в каталоге при закрытии очередного сокета).
    coord: Arc<std::sync::OnceLock<bmv_signal::Coordinator>>,
    /// Идентификатор этого узла как хоста (пока на сессию; позже — стабильный).
    host_id: String,
    /// Секрет владельца записи в каталоге (см. HostConfig.token). Отдаётся
    /// координатору в анонсе/bye — чужой не перепишет и не снимет наш хост.
    host_token: String,
    /// Подпись кода `host_id` сервером (HMAC из new_code). Координатор требует её
    /// при первом анонсе — хост не может придумать код сам (сервер = источник).
    host_code_sig: String,
    /// Анонсированные адреса хоста — чтобы ре-анонсить с тем же набором.
    host_endpoints: Arc<Mutex<Vec<String>>>,
    /// Активные гости: адрес пира → поколение сессии. Ключ — адрес: с одного
    /// `ip:port` не бывает двух гостей, переподключение ЗАМЕНЯЕТ запись, а не
    /// плодит; поколение (gen) не даёт «протухшей» сессии обнулить счётчик, если
    /// её место уже занял новый коннект. Счётчик гостей = размер карты.
    active_guests: Arc<Mutex<HashMap<SocketAddr, u64>>>,
    session_gen: Arc<AtomicU64>,
    /// Настройки хоста, меняемые НА ЛЕТУ (имя, лимит гостей, пароль — прямо во
    /// время работы, без перезапуска).
    host_name: Arc<Mutex<String>>,
    host_max: Arc<AtomicU32>,
    host_public: Arc<AtomicBool>,
    host_password: Arc<Mutex<String>>,
    /// Протокол хоста (plain/noise/noise-aes) — меняется на лету.
    host_protocol: Arc<Mutex<String>>,
    /// Хост активен? После остановки → false, и ЛЮБОЙ анонс становится no-op.
    /// Это глушит «воскрешение» из утёкших/завершающихся задач (heartbeat,
    /// выходящий гость): они больше не вернут снятый хост в каталог.
    host_active: Arc<AtomicBool>,
    /// Ворота на ОДНОВРЕМЕННЫЕ рукопожатия. Код публичного хоста виден в каталоге
    /// → любой может вывести его punch-токен и залить хаб PUNCH'ами: до
    /// MAX_HUB_PEERS (4096) гостей завелось бы, и каждый запускал бы полное
    /// Noise-рукопожатие (ChaCha20) ДО проверки лимита — пик CPU на 1-ядерном
    /// хосте, реальные гости голодают. Семафор ограничивает число ОДНОВРЕМЕННЫХ
    /// рукопожатий; лишние ждут очереди (рукопожатие — миллисекунды, легальный
    /// наплыв не страдает, а флуд не может выжрать ядро).
    handshake_gate: Arc<tokio::sync::Semaphore>,
    /// ОТДЕЛЬНЫЕ ворота для штрафной паузы на неверном пароле. Пауза обязана
    /// тормозить ПЕРЕБОР, но не легальных гостей: пока она занимала пермит
    /// `handshake_gate`, 64 неверных пароля в секунду выкупали все общие ворота, и
    /// подключиться не мог никто — защита от перебора работала как готовый DoS.
    penalty_gate: Arc<tokio::sync::Semaphore>,
}

/// Потолок одновременных рукопожатий гостей (анти-флуд, см. handshake_gate).
const HANDSHAKE_CONCURRENCY: usize = 64;

/// Сколько ждём ответа на пробу задержки. Полторы секунды: дальний хост через
/// половину планеты укладывается в ~300мс, поэтому всё, что дольше, — это уже
/// «не отвечает», а не «далеко». Ждать больше значит подвесить список.
const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

/// Как часто хост чинит запись в каталоге и держит NAT-дырку хаба открытой.
///
/// ЗДЕСЬ, а не у каждого, кто крутит цикл: дерево задач хоста написано ТРИЖДЫ
/// (десктоп — `bmv_desktop::hosting::serve_host`, телефоны — `bmv_host_start`,
/// терминал без экрана — свой цикл), и число было вписано в каждое место
/// отдельно. Рядом же стояло обещание «звать раз в ~15с», которого не выполнял
/// никто из троих: проверить его было не с чем, потому что сравнивать было не с
/// чем.
///
/// Тик держит две вещи сразу, и обе ломаются молча: реже — хост за NAT
/// становится недостижим для новых гостей (дырка закрылась), а его карточка в
/// каталоге показывает вчерашнее число гостей.
pub const HOST_HEARTBEAT: Duration = Duration::from_secs(10);

/// Пауза перед отказом на неверном пароле — тормоз перебора.
/// Секунда незаметна тому, кто просто опечатался, но подбор из миллиона
/// вариантов растягивает с часов на годы даже при полной загрузке ворот.
const WRONG_PASSWORD_DELAY: Duration = Duration::from_secs(1);

/// Краткая карточка протокола для UI/каталога.
pub struct ProtocolInfo {
    pub name: &'static str,
    pub encrypts: bool,
    pub available: bool,
}

impl BmvEngine {
    /// Собрать движок из конфига (единственная точка входа для оболочек).
    pub fn from_config(config: Config) -> Self {
        // Стабильный id из конфига, иначе случайный на сессию.
        let host_id = if config.host.id.is_empty() {
            bmv_common::ids::new_host_id(16)
        } else {
            config.host.id.clone()
        };
        // Секрет владельца: из конфига (стабильный между рестартами) или
        // случайный на сессию (защита в пределах жизни процесса).
        let host_token = if config.host.token.is_empty() {
            bmv_common::ids::new_host_id(32)
        } else {
            config.host.token.clone()
        };
        let default_proto = if config.default_protocol.is_empty() {
            bmv_config::DEFAULT_PROTOCOL.to_string() // единый дефолт проекта
        } else {
            config.default_protocol.clone()
        };
        BmvEngine {
            coord: Arc::new(std::sync::OnceLock::new()),
            host_name: Arc::new(Mutex::new(config.host.name.clone())),
            host_max: Arc::new(AtomicU32::new(config.host.max_guests)),
            host_public: Arc::new(AtomicBool::new(config.host.public)),
            host_password: Arc::new(Mutex::new(config.host.password.clone())),
            host_protocol: Arc::new(Mutex::new(default_proto)),
            host_active: Arc::new(AtomicBool::new(true)),
            host_code_sig: config.host.code_sig.clone(),
            stun: config.stun.resolve().into(), // диск читаем ЗДЕСЬ, и только здесь
            config,
            protocols: Arc::new(Registry::with_builtins()),
            host_id,
            host_token,
            host_endpoints: Arc::new(Mutex::new(Vec::new())),
            active_guests: Arc::new(Mutex::new(HashMap::new())),
            session_gen: Arc::new(AtomicU64::new(0)),
            handshake_gate: Arc::new(tokio::sync::Semaphore::new(HANDSHAKE_CONCURRENCY)),
            penalty_gate: Arc::new(tokio::sync::Semaphore::new(HANDSHAKE_CONCURRENCY)),
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn host_id(&self) -> &str {
        &self.host_id
    }

    // Здесь был `classify_nat` — «определить тип NAT для UI и раннего вывода
    // «нужен релей»». Ни одной оболочкой он не звался НИ РАЗУ: ни релея, ни
    // экрана диагностики в продукте нет, а строки "cone"/"symmetric" некому было
    // прочитать. Задел ценой в две секунды STUN на пустом месте.
    //
    // Понадобится показать тип NAT — это экран, а не метод: и тогда он придёт со
    // своим набором ответов, который заранее всё равно не угадать. Единственный
    // читатель `bmv_net::classify_mapping` был здесь — теперь та функция тоже без
    // вызовов (её судьба за владельцем bmv-net).

    // ── знакомство через КООРДИНАТОР (главный путь) ──────────────────────────

    /// Клиент координатора (первый из списка в конфиге) — ОДИН на движок,
    /// создаётся лениво и переиспользуется (персистентный WS-сокет).
    fn coordinator(&self) -> Result<bmv_signal::Coordinator> {
        if let Some(c) = self.coord.get() {
            return Ok(c.clone());
        }
        let base = self
            .config
            .coordinators
            .first()
            .ok_or_else(|| bmv_common::Error::Config("Не задан адрес сервера — укажите его во вкладке «Сервер».".into()))?;
        let fresh = bmv_signal::Coordinator::new(base)?;
        // Гонка двух первых вызовов безопасна: проигравший Coordinator дропается,
        // его супервизор ещё не стартовал (стартует лишь при первом использовании).
        let _ = self.coord.set(fresh);
        Ok(self.coord.get().expect("только что установлен").clone())
    }

    /// Подписка на подсказки координатора «проверь соседа» (см.
    /// `bmv_signal::Coordinator::peer_check`). `None` — координатор не настроен:
    /// прямое прощание и таймаут тишины работают и без него, это ТРЕТИЙ слой, а
    /// не единственный.
    pub fn peer_check(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        self.coordinator().ok().map(|c| c.peer_check())
    }

    /// Гонять подсказки координатора в канал СЕССИИ, пока сессия жива.
    ///
    /// Никогда не завершается сама — её место в `tokio::select!` рядом с самой
    /// сессией: кончилась сессия, кончился и ретранслятор (без spawn'а и без
    /// парного abort'а, которые пришлось бы гасить руками).
    pub async fn relay_peer_checks(link: &dyn Link, rx: Option<tokio::sync::watch::Receiver<u64>>) {
        if let Some(mut rx) = rx {
            while rx.changed().await.is_ok() {
                link.check_peer_now();
            }
        }
        std::future::pending::<()>().await
    }

    /// Проверить, что координатор жив.
    pub async fn coordinator_health(&self) -> Result<()> {
        self.coordinator()?.health().await
    }

    /// Круг до координатора в мс, `None` — ещё не мерили или связь оборвана.
    ///
    /// Именно ЗАМЕР, а не время вызова `coordinator_health`: тот читает флаг
    /// «сокет жив» и возвращается мгновенно, поэтому раньше на экране всегда
    /// стоял ноль.
    pub fn coordinator_rtt(&self) -> Option<u32> {
        self.coordinator().ok().and_then(|c| c.rtt_ms())
    }

    /// ГОСТЬ: получить список хостов из каталога.
    ///
    /// Аргумента `public_only` больше нет: каталог координатора и так состоит
    /// ТОЛЬКО из публичных хостов, а сам флаг никогда не доезжал до провода.
    /// Скрытый хост ищется по коду — `guest_resolve`, а не фильтром.
    pub async fn guest_list(&self, country: Option<String>) -> Result<Vec<bmv_signal::HostInfo>> {
        let coord = self.coordinator()?;
        coord.directory(&bmv_signal::Filter { country }).await
    }

    /// ГОСТЬ: найти хост по коду (id) — в т.ч. СКРЫТУЮ сеть (её нет в каталоге).
    /// None — код не найден. Дальше подключение обычное (guest_establish).
    pub async fn guest_resolve(&self, id: &str) -> Result<Option<bmv_signal::HostInfo>> {
        self.coordinator()?.resolve(id).await
    }

    /// ХОСТ: попросить у сервера новый код (сервер генерит И ПОДПИСЫВАЕТ, не
    /// клиент). Возвращает (код, подпись) — оба нужны, чтобы потом анонсироваться.
    pub async fn host_new_code(&self) -> Result<(String, String)> {
        let nc = self.coordinator()?.new_code().await?;
        Ok((nc.code, nc.sig))
    }

    /// ГОСТЬ: наблюдать за каталогом (long-poll до изменения). since — версия из
    /// прошлого ответа (0 = ответить сразу). Крутить в цикле → живой каталог.
    pub async fn guest_watch(
        &self,
        country: Option<String>,
        since: u64,
    ) -> Result<bmv_signal::DirectoryUpdate> {
        let coord = self.coordinator()?;
        coord.directory_watch(&bmv_signal::Filter { country }, since).await
    }

    /// ГОСТЬ: установить зашифрованный канал к хосту — на ОДНОМ сокете: собрать
    /// свои кандидаты, взять у координатора адреса хоста, пробить NAT, поднять
    /// протокол. Возвращает (адрес пира, готовый Link).
    ///
    /// Один сокет на всё критичен: адрес, что мы сообщаем координатору (srflx),
    /// должен быть портом, с которого мы реально пробиваем NAT.
    /// `password`: Some — для запароленного хоста (шлём auth-кадр первым пакетом);
    /// None — обычный хост (путь не меняется).
    pub async fn guest_establish(
        &self,
        host_id: &str,
        password: Option<&str>,
        protocol: Option<&str>,
    ) -> Result<(SocketAddr, Box<dyn Link>)> {
        let coord = self.coordinator()?;

        // Биндим сокет и собираем СВОИ кандидаты (local + srflx) — на том же
        // порту, с которого будем пробивать. Отдаём их координатору: хост за NAT
        // пробьёт нам навстречу (без этого его не достать).
        let ep = UdpEndpoint::bind("0.0.0.0:0".parse().unwrap()).await?;
        let port = ep.local_addr()?.port();
        let reflexive = ep.reflexive(&self.stun, Duration::from_secs(3)).await.ok();
        let candidates = my_endpoints(port, reflexive);

        let resp = coord
            .connect(&bmv_signal::GuestConnect {
                host_id: host_id.to_string(),
                candidates,
            })
            .await?;
        let peers = parse_addrs(&resp.host_endpoints);
        if peers.is_empty() {
            return Err(bmv_common::Error::Net("У этого хоста нет рабочего адреса — выберите другой.".into()));
        }

        // Пробиваем NAT к хосту и поднимаем протокол (гость — инициатор).
        // Токены пробивания выводятся из host_id (не общий ASCII-маркер).
        // Окно щедрое: между двумя мобильными NAT пробитие может занять секунды.
        let tokens = bmv_net::PunchTokens::for_host(host_id);
        let (peer, primed) = ep.hole_punch(&peers, Duration::from_secs(12), tokens).await?;
        // Протокол ДОЛЖЕН совпадать с хостом (гость берёт его из каталога).
        // Пусто/незнакомо → дефолт проекта, и это решает `protocol_by_name`.
        let proto = self.protocol_by_name(protocol.unwrap_or(""));
        let link = ep.connect_primed(peer, primed, tokens);
        let plink = proto.connect_guest(Box::new(link)).await?;
        let link: Box<dyn Link> = Box::new(bmv_common::KeepaliveLink::new(plink));
        // Если хост запаролен — первым делом шлём auth-кадр (иначе хост нас закроет).
        if let Some(pw) = password {
            link.send(&auth_frame(pw)).await?;
        }
        Ok((peer, link))
    }

    /// ГОСТЬ (эхо-проверка): установить канал, послать сообщение, вернуть эхо.
    pub async fn guest_connect_run(
        &self,
        host_id: &str,
        msg: &[u8],
    ) -> Result<(SocketAddr, Vec<u8>)> {
        let (peer, link) = self.guest_establish(host_id, None, None).await?;
        link.send(msg).await?;
        let echo = link.recv().await?;
        Ok((peer, echo))
    }

    // Гость-туннель оркестрируется ПЛАТФОРМОЙ (шеллом): она создаёт TUN и зовёт
    // `bmv_tunnel::run_guest(device, link)`, где link берётся из guest_establish.
    // Ядро не создаёт TUN само (это платформо-зависимо).

    // ── реальный UDP-путь хоста/гостя (сквозное соединение) ──────────────────

    /// Задержка до хоста БЕЗ подключения к нему — чтобы выбирать из списка не
    /// вслепую. `None` = не ответил за отведённое время (недостижим отсюда).
    ///
    /// Проба не создаёт на хосте ни сессии, ни записи (см. `bmv_net::probe_rtt`),
    /// поэтому её можно звать для нескольких хостов подряд.
    ///
    /// Адреса — из карточки каталога, то есть ЧУЖИЕ СЛОВА, ровно как у пробития.
    /// Поэтому и фильтр тот же (`parse_addrs`): это была единственная дверь, через
    /// которую адрес от координатора уходил в сокет непроверенным, а зовут её все
    /// четыре оболочки, причём в цикле раз в секунду, пока открыта карточка.
    /// Против честного координатора ничего не менялось — но эшелон обороны стоит
    /// одну строку, а без него подменённая карточка превращала каждое запущенное
    /// приложение в сканер чужой локальной сети и облачных метаданных.
    pub async fn probe_host_rtt(&self, host_id: &str, endpoints: &[String]) -> Option<u32> {
        let targets: Vec<String> = parse_addrs(endpoints).iter().map(|a| a.to_string()).collect();
        bmv_net::probe_rtt(host_id, &targets, PROBE_TIMEOUT)
            .await
            .map(|d| d.as_millis().min(u32::MAX as u128) as u32)
    }

    /// ХОСТ: поднять хаб-сокет (мультигость), собрать кандидаты, анонсировать.
    /// Возвращает хаб (держать!), id и список анонсированных адресов.
    pub async fn host_bind_announce(
        &self,
    ) -> Result<(std::sync::Arc<bmv_net::UdpHub>, String, Vec<String>)> {
        // STUN делаем ВНУТРИ bind (до старта демультиплексора) — иначе demux
        // съедает STUN-ответ и хост за NAT узнаёт только свой LAN-адрес.
        // Токены пробивания и пробы задержки — из СВОЕГО host_id (гость выведет те же).
        let tokens = bmv_net::PunchTokens::for_host(&self.host_id);
        let (hub, reflexive) = bmv_net::UdpHub::bind_reflexive(
            "0.0.0.0:0".parse().unwrap(), &self.stun, Duration::from_secs(4), tokens,
        )
        .await?;
        let port = hub.local_addr()?.port();
        let endpoints = my_endpoints(port, reflexive);

        // Запоминаем адреса — по ним пойдут все ре-анонсы (heartbeat + при
        // изменении числа гостей).
        *self.host_endpoints.lock() = endpoints.clone();
        self.announce_state().await?;
        Ok((hub, self.host_id.clone(), endpoints))
    }

    /// Собрать анонс из конфига + ЖИВОГО числа гостей + рантайм-настроек.
    fn build_announce(&self) -> bmv_signal::HostAnnounce {
        // Пароль ⇒ сеть ВСЕГДА скрытая. Паролем закрываются от посторонних, а
        // публичная карточка светит существование сети, её страну и адрес всему
        // каталогу — то есть ровно тем, от кого закрывались. Правило живёт ЗДЕСЬ,
        // в сборке анонса: через неё проходят все платформы и все пути (старт,
        // смена пароля на лету, восстановление после реконнекта), поэтому
        // рассогласования между оболочками быть не может.
        let has_password = !self.host_password.lock().is_empty();
        let public = self.host_public.load(Ordering::SeqCst) && !has_password;
        bmv_signal::HostAnnounce {
            id: self.host_id.clone(),
            token: self.host_token.clone(),
            name: self.host_name.lock().clone(),
            endpoints: self.host_endpoints.lock().clone(),
            country: self.config.host.country_hint.clone(),
            public,
            max_guests: self.host_max.load(Ordering::SeqCst),
            guests: self.active_guests.lock().len() as u32,
            has_password,
            protocol: self.host_protocol.lock().clone(),
            code_sig: self.host_code_sig.clone(),
        }
    }

    /// ХОСТ: сменить ИМЯ на лету (сразу видно в каталоге).
    pub async fn host_set_name(&self, name: &str) -> Result<()> {
        *self.host_name.lock() = name.to_string();
        self.announce_state().await
    }

    /// ХОСТ: сменить ПРОТОКОЛ на лету (влияет на новых гостей).
    pub async fn host_set_protocol(&self, protocol: &str) -> Result<()> {
        *self.host_protocol.lock() = protocol.to_string();
        self.announce_state().await
    }

    /// ЕДИНСТВЕННАЯ ДВЕРЬ ОТ ИМЕНИ ПРОТОКОЛА К САМОМУ ПРОТОКОЛУ.
    ///
    /// Пусто — имя не назвали (пустая настройка из оболочки, хост не объявил
    /// протокол в каталоге): берём ЕДИНЫЙ дефолт проекта молча. Раньше пустая
    /// строка у гостя означала «noise», а у хоста «noise-obfs» — стороны уходили
    /// в разные протоколы, и человек видел 12 секунд тишины вместо ошибки.
    ///
    /// Незнакомое имя — рассогласование версий (хост объявил протокол, которого
    /// нет в этой сборке): берём тот же дефолт — его возьмёт и вторая сторона,
    /// поэтому шанс договориться остаётся, — но ГРОМКО, а не молча.
    ///
    /// Последний шаг — `expect`, а не «первый доступный из реестра»: реестр
    /// собирается из встроенного списка (`Registry::with_builtins`), дефолт
    /// проекта в нём есть всегда, и это проверяет тест
    /// `default_protocol_is_one_for_everyone`. Прежний «фолбэк» изображал выбор
    /// там, где выбора нет, и при этом молча подсовывал «первый доступный» —
    /// ровно то, от чего эта функция и заводилась.
    fn protocol_by_name(&self, name: &str) -> Arc<dyn Protocol> {
        if let Some(p) = self.protocols.get(name) {
            return p;
        }
        self.protocols
            .get(bmv_config::DEFAULT_PROTOCOL)
            .expect("дефолт проекта обязан быть в реестре (Registry::with_builtins)")
    }

    /// Протокол ЭТОГО узла по настройке — уже разобранный, а не строка.
    ///
    /// Через него же идут `active_protocol` (что показать человеку) и
    /// `demo_loopback` (чем проверять связь): раньше это были три разных ответа
    /// на один вопрос. При пустой настройке экран говорил «noise», сессия шла
    /// «noise-obfs», а демо — вообще «plain», то есть БЕЗ ШИФРА.
    fn default_proto(&self) -> Arc<dyn Protocol> {
        self.protocol_by_name(&self.config.default_protocol)
    }

    /// ХОСТ: сменить лимит гостей НА ЛЕТУ (сразу видно в каталоге).
    pub async fn host_set_max_guests(&self, max: u32) -> Result<()> {
        self.host_max.store(max, Ordering::SeqCst);
        self.announce_state().await
    }

    /// ХОСТ: сменить пароль НА ЛЕТУ (пусто = снять пароль). Влияет на новых гостей.
    pub async fn host_set_password(&self, password: &str) -> Result<()> {
        *self.host_password.lock() = password.to_string();
        self.announce_state().await
    }

    /// ХОСТ: сменить ВИДИМОСТЬ на лету. public=false → хост пропадает из общего
    /// каталога (остаётся доступен по коду); true → снова появляется. Мгновенно.
    pub async fn host_set_public(&self, public: bool) -> Result<()> {
        self.host_public.store(public, Ordering::SeqCst);
        self.announce_state().await
    }

    /// ХОСТ: анонсировать ТЕКУЩЕЕ состояние (адреса + живое число гостей).
    /// announce идемпотентен: обновляет запись ИЛИ создаёт заново. НО если хост
    /// уже остановлен (`host_active=false`) — НИЧЕГО не делаем: иначе утёкший
    /// heartbeat или выходящий гость воскресили бы снятый хост в каталоге.
    pub async fn announce_state(&self) -> Result<()> {
        if !self.host_active.load(Ordering::SeqCst) {
            return Ok(());
        }
        let ann = self.build_announce();
        let coord = self.coordinator()?;
        // ПОВТОРНАЯ проверка ПЕРЕД самой отправкой: между первой проверкой и этим
        // местом мог пройти host_deannounce (он ставит host_active=false, ЗАТЕМ
        // шлёт bye), и тогда анонс уже незачем отправлять.
        //
        // ЧЕСТНО ПРО ПОТОЛОК: это СУЖЕНИЕ окна, а не запрет. Задачи идут на
        // многопоточном рантайме, и нашу задачу может снять с ядра прямо здесь —
        // между этой проверкой и первой строкой `announce`. Тогда порядок на
        // проводе выйдет «bye, затем host», и хост вернётся в каталог призраком:
        // клиент координатора ещё и запоминает последний анонс, чтобы повторить
        // его на переподключении. Прежний комментарий обещал тут гарантию
        // («announce уходит РАНЬШЕ bye»), которой нет ни у одной из двух
        // проверок. Настоящая гарантия — в клиенте координатора: после `bye`
        // сокет обязан отвергать анонсы этой записи насовсем; отсюда, снаружи,
        // такое не выражается. Практическая цена окна мала: heartbeat остановлен
        // раньше bye (это делают все оболочки), поэтому попасть в него может
        // только анонс, начатый в ту же миллисекунду.
        if !self.host_active.load(Ordering::SeqCst) {
            return Ok(());
        }
        // Живость хоста теперь — сам WS-сокет к координатору (закрылся → убрали
        // мгновенно). Отдельная UDP-проверка больше не нужна.
        coord.announce(&ann).await?;
        Ok(())
    }

    // Здесь был `host_reannounce(endpoints)` — «совместимость: ре-анонс с явными
    // адресами». Совместимость с НИКЕМ: ни одна оболочка его не звала, а адреса
    // хост запоминает сам в `host_bind_announce`. Ручка позволяла подменить набор
    // анонсируемых адресов извне — то есть единственное, что она могла сделать,
    // это разойтись с тем, на чём хост реально слушает.

    /// ХОСТ: периодический тик — держит NAT-дырку hub-сокета открытой (иначе
    /// хост за NAT становится недостижим для новых гостей) и обновляет запись в
    /// каталоге. Звать раз в `HOST_HEARTBEAT` из цикла приёма гостей.
    pub async fn host_heartbeat(&self, hub: &bmv_net::UdpHub) -> Result<()> {
        hub.nat_keepalive(&self.stun).await;
        self.announce_state().await
    }

    /// ХОСТ: цикл ВСТРЕЧНОГО ПРОБИТИЯ. Ждёт от координатора ждущих гостей и шлёт
    /// им PUNCH, открывая свой NAT навстречу (иначе хост за NAT недостижим).
    /// Крутить фоном рядом с циклом приёма гостей.
    ///
    /// Гость приходит ПУШЕМ — координатор сам толкает его кадром в сокет, — так
    /// что здесь нет ни опроса, ни «версии», ни таймаута: `next_guest` ждёт
    /// ровно до появления гостя. Раньше на этом месте стоял `pending(id, since)`,
    /// возвращавший список и нулевую «версию», которую цикл прилежно клал обратно
    /// в `since`, откуда её выбрасывали, — вся конструкция изображала HTTP-опрос,
    /// которого нет уже давно.
    pub async fn host_serve_punch(&self, hub: std::sync::Arc<bmv_net::UdpHub>) -> Result<()> {
        let coord = self.coordinator()?;
        loop {
            let cands = match coord.next_guest().await {
                Ok(c) => c,
                // Связь оборвалась — супервизор клиента её восстановит, а мы
                // подождём, чтобы не крутить пустой цикл на полной скорости.
                Err(_) => {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
            };
            let addrs = parse_addrs(&cands);
            if addrs.is_empty() {
                continue;
            }
            // Встречный PUNCH на ВСЁ окно пробития гостя (~12с). Раньше было
            // ~3с и не перекрывало 12с-окно гостя — для обратного пробития
            // (строгий хост × мягкий гость) это критично: гостю нужно успеть
            // выучить реальный порт хоста из этих PUNCH и простучать назад,
            // пока его окно открыто. Лишние PUNCH безвредны (держат NAT-дырку).
            let hub2 = hub.clone();
            tokio::spawn(async move {
                for _ in 0..48 {
                    hub2.punch(&addrs).await;
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            });
        }
    }

    /// ХОСТ: убрать себя из каталога СРАЗУ и НАВСЕГДА заглушить этот движок
    /// (host_active=false) — чтобы никакая утёкшая/выходящая задача его не
    /// воскресила. Zовём при остановке раздачи.
    pub async fn host_deannounce(&self) -> Result<()> {
        self.host_active.store(false, Ordering::SeqCst);
        self.coordinator()?.bye().await
    }

    /// Узнать свой внешний IP через координатор (для UI, без сторонних сайтов).
    pub async fn my_ip(&self) -> Result<String> {
        self.coordinator()?.my_ip().await
    }

    /// ХОСТ: обслужить ОДНОГО гостя (сырой канал от хаба) до обрыва сессии.
    /// Поднимает протокол (шифрование) + keepalive, затем в зависимости от режима
    /// либо туннель (userspace-стек, без прав), либо эхо (для проверки связи).
    ///
    /// Счётчик гостей честный: слот занимается ТОЛЬКО после успешного рукопожатия
    /// (фантомные PUNCH/ретраи не считаются) и сразу ре-анонсится — каталог видит
    /// «X из Y» мгновенно; на выходе слот освобождается и снова ре-анонс.
    /// При переполнении — вежливый отказ (BYE), гость видит EOF, а не зависание.
    pub async fn host_run_session(
        &self,
        peer: SocketAddr,
        raw: Box<dyn Link>,
        tunnel: bool,
    ) -> Result<()> {
        let proto = self.protocol_by_name(&self.host_protocol.lock().clone());
        // Ворота анти-флуда: не даём флуду PUNCH'ей запустить тысячи параллельных
        // рукопожатий и выжрать CPU. Пермит держим ТОЛЬКО на время рукопожатия
        // (миллисекунды), затем отпускаем — сессия дальше идёт без него.
        let plink = {
            let _permit = self.handshake_gate.acquire().await.map_err(|_| bmv_common::Error::other("Приложение закрывается — подключение отменено."))?;
            proto.connect_host(raw).await? // рукопожатие ДО учёта
        };
        let link: Arc<dyn Link> = Arc::new(bmv_common::KeepaliveLink::new(plink));

        // ПАРОЛЬ: если задан — первый пакет гостя должен быть верным auth-кадром.
        // Неверный/отсутствует → вежливо закрываем, слот не занимаем.
        let pw = self.host_password.lock().clone();
        if !pw.is_empty() {
            let ok = matches!(
                tokio::time::timeout(Duration::from_secs(6), link.recv()).await,
                Ok(Ok(frame)) if verify_auth(&frame, &pw)
            );
            if !ok {
                // Задержка перед отказом — иначе пароль перебирается на полной
                // скорости: неудачная попытка стоила только рукопожатия, а их
                // разрешено 64 одновременно. Пауза держит занятым слот ворот,
                // поэтому темп перебора падает на порядки. Легального гостя это
                // не задевает: он платит её только когда ошибся паролем.
                // Пауза ФИКСИРОВАННАЯ: случайная ничего не добавляет (сравнение
                // и так постоянного времени), а плавающий ответ лишь мешал бы
                // отличить «не тот пароль» от «сеть тормозит».
                // Ворота — ОТДЕЛЬНЫЕ (см. penalty_gate): темп перебора ограничен
                // по-прежнему, но легальные гости в это время подключаются.
                let _slow = self.penalty_gate.acquire().await;
                tokio::time::sleep(WRONG_PASSWORD_DELAY).await;
                let _ = link.close().await;
                return Err(bmv_common::Error::Protocol("Неверный пароль.".into()));
            }
        }

        // Слот занимаем ГАРДОМ: он снимет запись на ЛЮБОМ выходе, включая панику и
        // отмену задачи (а сессии гостей во всех оболочках живут в tokio::spawn).
        let Some(_slot) = GuestSlot::take(self, peer) else {
            let _ = link.close().await;
            return Err(bmv_common::Error::Net("На этом хосте нет свободных мест — выберите другой.".into()));
        };
        let _ = self.announce_state().await; // каталог видит нового гостя сразу

        // Подсказки координатора «проверь соседа» — на всё время сессии (третий
        // слой обнаружения ухода: гость мог быть убит, и прощание послать некому).
        let hints = Self::relay_peer_checks(&*link, self.peer_check());
        let res = tokio::select! {
            r = async {
                if tunnel {
                    bmv_tunnel::run_host(link.clone()).await
                } else {
                    echo_session(&*link).await
                }
            } => r,
            // Ретранслятор не завершается сам — эта ветка недостижима.
            _ = hints => Ok(()),
        };

        drop(_slot); // место освобождаем ДО анонса, чтобы каталог увидел уже новое число
        let _ = self.announce_state().await; // гость ушёл — счётчик вниз сразу
        res
    }

    /// Список протоколов с флагами — для UI (замочек, «доступен/нет»).
    pub fn protocols(&self) -> Vec<ProtocolInfo> {
        self.protocols
            .names()
            .into_iter()
            .filter_map(|name| self.protocols.get(name))
            .map(|p| ProtocolInfo {
                name: p.name(),
                encrypts: p.encrypts(),
                available: p.available(),
            })
            .collect()
    }

    /// Список протоколов для ПОКАЗА: свой первым, остальные доступные следом.
    ///
    /// Это витрина, а не фолбэк: перебора протоколов при подключении нет и
    /// никогда не было — обе стороны берут РОВНО ОДНО имя (см.
    /// `protocol_by_name`), потому что договориться на лету по UDP не с кем.
    /// Единственный, кто это зовёт, — терминальная команда `protocols`.
    pub fn connect_order(&self) -> Vec<Arc<dyn Protocol>> {
        self.protocols.display_order(self.active_protocol())
    }

    /// Имя протокола, которым этот узел РЕАЛЬНО работает, — для вывода человеку.
    ///
    /// Берётся из того же разбора, что и сама сессия. Раньше здесь стоял «первый
    /// доступный из порядка фолбэка», и при пустой настройке экран честно писал
    /// «noise», пока хост и гость поднимали «noise-obfs».
    pub fn active_protocol(&self) -> &'static str {
        self.default_proto().name()
    }

    /// Узнать свой внешний адрес через STUN-пул (из файла/конфига или встроенный).
    pub async fn external_addr(&self) -> Result<SocketAddr> {
        bmv_net::reflexive_addr(&self.stun, Duration::from_secs(5)).await
    }

    /// ДЕМО: прогнать пакет host↔guest через выбранный протокол по in-memory
    /// каналу (loopback, без сети). Доказывает связку конфиг → реестр → протокол.
    pub async fn demo_loopback(&self, payload: &[u8]) -> Result<Vec<u8>> {
        use bmv_common::wire::memory_pair;

        // ТОТ ЖЕ разбор имени, что и у боевой сессии. Здесь стоял свой:
        // «протокол из настройки, иначе plain» — то есть на незнакомом или
        // пустом имени проверка связи шла БЕЗ ШИФРА, а рядом печаталось имя
        // из `active_protocol`. Проверка, которая проверяет не то, что работает,
        // хуже отсутствующей.
        let proto = self.default_proto();

        let (a, b) = memory_pair(16);

        // Рукопожатия хоста и гостя идут ОДНОВРЕМЕННО (для Noise это обязательно:
        // responder ждёт первое сообщение initiator'а — их нельзя запускать по
        // очереди, иначе взаимная блокировка).
        let (host_link, guest_link) =
            tokio::join!(proto.connect_host(a), proto.connect_guest(b));
        let host_link = host_link?;
        let guest_link = guest_link?;

        // Гость шлёт пакет → хост принимает и эхо-ответ → гость читает.
        guest_link.send(payload).await?;
        let got = host_link.recv().await?;
        host_link.send(&got).await?;
        let echo = guest_link.recv().await?;
        Ok(echo)
    }
}

/// ЗАНЯТЫЙ СЛОТ ГОСТЯ. Живёт, пока живёт сессия; на Drop снимает запись из
/// `active_guests`.
///
/// Гард, а не пара «вставил/удалил» вокруг сессии: снятие «в конце функции»
/// НЕ ВЫПОЛНЯЕТСЯ при панике и при отмене задачи, а гостя каждая оболочка
/// обслуживает в `tokio::spawn` — то есть один свёрнутый экран или одна паника в
/// туннеле оставляли запись навсегда. Счётчик полз вверх, и после `max_guests`
/// таких случаев хост отказывал ВСЕМ, показывая «X из X» при нуле реальных гостей.
/// Заодно это единственное место учёта — раньше блок дублировался дважды.
struct GuestSlot {
    guests: Arc<Mutex<HashMap<SocketAddr, u64>>>,
    peer: SocketAddr,
    /// Поколение сессии: снимаем запись ТОЛЬКО если её не занял новый коннект с
    /// того же адреса (иначе протухшая сессия обнуляла бы живую).
    gen: u64,
}

impl GuestSlot {
    /// Занять слот. `None` — хост заполнен (лимит гостей).
    fn take(engine: &BmvEngine, peer: SocketAddr) -> Option<GuestSlot> {
        let gen = engine.session_gen.fetch_add(1, Ordering::SeqCst);
        let max = engine.host_max.load(Ordering::SeqCst);
        let mut g = engine.active_guests.lock();
        if max > 0 && !g.contains_key(&peer) && g.len() as u32 >= max {
            return None;
        }
        g.insert(peer, gen);
        Some(GuestSlot { guests: engine.active_guests.clone(), peer, gen })
    }
}

impl Drop for GuestSlot {
    fn drop(&mut self) {
        let mut g = self.guests.lock();
        if g.get(&self.peer) == Some(&self.gen) {
            g.remove(&self.peer);
        }
        // Ре-анонс отсюда невозможен (Drop синхронный, а анонс — сеть). При
        // штатном выходе его делает host_run_session; при отмене/панике каталог
        // подтянется ближайшим heartbeat'ом — числа сходятся с задержкой в тик,
        // но НЕ врут навсегда, как раньше.
    }
}

/// КАК УЗЕЛ НАЗЫВАЕТ СЕБЯ ВТОРОЙ СТОРОНЕ: домашний адрес плюс внешний, если STUN
/// его добыл. Без хвостов и без повторов.
///
/// Одно правило на обе стороны. Хост объявляет этот список каталогом, гость
/// отдаёт его координатору для встречного пробития — дело одно и то же, а
/// написано было дважды, и копии успели разойтись: у хоста провал STUN писался в
/// журнал, у гостя проглатывался молча. А это ровно тот случай, который потом
/// выглядит как «пробитие не работает у половины людей»: без внешнего адреса
/// узел за NAT недостижим, и знать об этом надо ОБОИМ.
///
/// `None` в `reflexive` — STUN не ответил. Список от этого не пустеет: домашний
/// адрес остаётся, и пир в той же локальной сети достучится.
fn my_endpoints(port: u16, reflexive: Option<SocketAddr>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(ip) = bmv_net::local_ip() {
        out.push(format!("{ip}:{port}"));
    }
    // STUN не ответил — молча: сказать об этом некому и незачем, а сам факт
    // «из интернета меня не достать» человек увидит по несостоявшемуся подключению.
    if let Some(ext) = reflexive {
        out.push(ext.to_string());
    }
    out.sort();
    out.dedup();
    out
}

/// Сколько адресов одного пира вообще имеет смысл пробивать. У живого участника
/// их два (домашний и внешний), координатор режет список до восьми. Свой потолок
/// нужен на случай ЧУЖОГО координатора: по каждому адресу уходит 48 пачек UDP с
/// шагом 250мс, и длинный список превращает хост в веерный флудер.
const MAX_PUNCH_TARGETS: usize = 8;

/// Разобрать список строк "ip:port" в адреса ДЛЯ ПРОБИВАНИЯ: выбросить
/// неразборчивые и внутренние, схлопнуть дубликаты, ограничить число целей.
///
/// Фильтр здесь, а не у вызывающих, потому что через эту функцию проходят ОБА
/// пути: адреса гостя (их хосту называет координатор) и адреса хоста (их
/// называет гостю тот же координатор). В обе стороны это чужие слова, и по
/// каждому адресу уходит 12 секунд UDP-пачек — без фильтра получается сканер LAN
/// и облачных метаданных, оплаченный чужой машиной. Правило общее с туннелем
/// (`bmv_tunnel::punch_target_allowed`): приватка разрешена (пир в той же
/// локальной сети — рабочий случай), петля/link-local/мультикаст — нет.
fn parse_addrs(list: &[String]) -> Vec<SocketAddr> {
    let mut out: Vec<SocketAddr> = Vec::new();
    for s in list {
        let Ok(addr) = s.parse::<SocketAddr>() else { continue };
        if !bmv_tunnel::punch_target_allowed(&addr) {
            continue;
        }
        if !out.contains(&addr) {
            out.push(addr);
        }
        if out.len() >= MAX_PUNCH_TARGETS {
            break;
        }
    }
    out
}

/// Собрать auth-кадр гостя: маркер + пароль (UTF-8).
fn auth_frame(password: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(1 + password.len());
    v.push(AUTH_MARKER);
    v.extend_from_slice(password.as_bytes());
    v
}

/// Проверить auth-кадр от гостя против пароля хоста. Сам пароль сравниваем в
/// ПОСТОЯННОЕ время (не выходим на первом несовпавшем байте) — чтобы не давать
/// тайминг-подсказок для перебора. Канал уже зашифрован Noise, но пусть будет.
fn verify_auth(frame: &[u8], password: &str) -> bool {
    !frame.is_empty() && frame[0] == AUTH_MARKER && ct_eq(&frame[1..], password.as_bytes())
}

/// Сравнение байт за постоянное время (для секретов). Разная длина → сразу false
/// (длина пароля и так не секрет); одинаковая — без ранних выходов.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Эхо-сессия (проверка связи): вернуть всё, что прислали, до EOF.
async fn echo_session(link: &dyn Link) -> Result<()> {
    loop {
        let pkt = link.recv().await?;
        if pkt.is_empty() {
            return Ok(());
        }
        link.send(&pkt).await?;
    }
}

#[cfg(test)]
mod announce_tests {
    use super::*;
    use bmv_protocol::Protocol;

    /// Движок, который НЕ ХОДИТ к координатору: `host_active=false` глушит любой
    /// анонс (см. `announce_state`). Иначе каждый тест сессии ждал бы сеть по 10с
    /// на пустом месте.
    fn offline_engine(cfg: Config) -> BmvEngine {
        let eng = BmvEngine::from_config(cfg);
        eng.host_active.store(false, Ordering::SeqCst);
        eng
    }

    /// ВТОРОЙ СЛОЙ ПРОЩАНИЯ, ПОСЛЕДНЕЕ ЗВЕНО. Координатор увидел, что сосед по
    /// паре отвалился, и прислал `peercheck` (серверная половина —
    /// `a_dying_host_makes_the_coordinator_hint_its_guest`, разбор кадра —
    /// `a_peercheck_frame_becomes_a_liveness_probe` в bmv-signal). Дальше
    /// подсказку в канал СЕССИИ обязана донести вот эта функция.
    ///
    /// Её не касался ни один тест: `relay_peer_checks` можно было удалить
    /// целиком — подсказка не доходила бы никогда, обнаружение обрыва
    /// откатывалось бы к обычной тишине (8с вместо 4с), и падало бы ноль тестов.
    /// Здесь пир «убит» (молчит, попрощаться не мог) — ждать его обязаны по
    /// КОРОТКОМУ сроку.
    #[tokio::test(start_paused = true)]
    async fn a_coordinator_hint_reaches_the_live_session() {
        let (a, b) = bmv_common::wire::memory_pair(64);
        let link: Arc<dyn Link> = Arc::new(bmv_common::KeepaliveLink::new(a));
        let _killed_peer = b; // конец провода держим, но молчим — как убитое приложение

        let (hints, rx) = tokio::sync::watch::channel(0u64);
        let started = tokio::time::Instant::now();
        let session = {
            let link = link.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                tokio::select! {
                    r = link.recv_into(&mut buf) => (r.unwrap(), tokio::time::Instant::now()),
                    // Ровно как в бою: ретранслятор стоит в select! рядом с сессией.
                    _ = BmvEngine::relay_peer_checks(&*link, Some(rx)) => unreachable!(),
                }
            })
        };
        tokio::task::yield_now().await;
        hints.send_modify(|v| *v += 1); // координатор: «проверь соседа»

        let (alive, ended) = session.await.unwrap();
        let took = ended.duration_since(started);
        assert!(!alive, "молчащий пир обязан кончиться EOF");
        // 8с — обычный срок тишины (`DEAD_AFTER`), 4с — ужатый по подсказке
        // (`PEER_CHECK_GRACE`); оба живут в bmv-common::wire.
        assert!(took < Duration::from_secs(8), "подсказка не доехала до канала: ждали {took:?} ≈ обычную тишину");
        assert!(took >= Duration::from_secs(4), "срок ужат сильнее, чем живой пир успевает ответить ({took:?})");
    }

    /// ПРОБА ЗАДЕРЖКИ НЕ ОТПРАВЛЯЕТСЯ НА ПЕТЛЮ.
    ///
    /// `probe_host_rtt` была ЕДИНСТВЕННЫМ путём, где адрес из карточки каталога
    /// уходил в сокет без проверки: пробитие свои цели чистит через `parse_addrs`,
    /// а проба брала список как есть. Зовут её все четыре оболочки, причём в
    /// цикле раз в секунду, пока открыта карточка хоста, — то есть подменённая
    /// карточка превращала бы каждое запущенное приложение в исправный сканер
    /// петли, локальной сети и облачных метаданных (169.254.169.254) чужими
    /// руками. Это эшелон обороны: против честного координатора ничего не
    /// менялось, но полагаться на его честность здесь незачем.
    ///
    /// Проверяем не возвращаемое значение (оно и так `None` — на петле никто не
    /// ответит), а ФАКТ ОТПРАВКИ: слушатель на 127.0.0.1 обязан не получить ни
    /// байта.
    #[tokio::test]
    async fn a_probe_never_reaches_the_loopback() {
        let eng = offline_engine(Config::default());
        // «Жертва» из подменённой карточки каталога — настоящий сокет на петле.
        let victim = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = victim.local_addr().unwrap().to_string();

        let rtt = eng.probe_host_rtt("host-id", std::slice::from_ref(&addr)).await;
        assert_eq!(rtt, None, "с петли пришёл ответ — значит проба туда сходила");

        let mut buf = [0u8; 128];
        let got = tokio::time::timeout(Duration::from_millis(400), victim.recv_from(&mut buf)).await;
        assert!(got.is_err(), "на петлю ({addr}) улетел пакет пробы: {} байт", got.map(|r| r.map(|(n, _)| n).unwrap_or(0)).unwrap_or(0));
    }

    /// Поднять гостя на другом конце канала (то же рукопожатие, что в бою).
    async fn guest_side(link: Box<dyn Link>, proto: &str) -> Box<dyn Link> {
        let p = if proto == "noise" { bmv_protocol::Noise::chacha() } else { bmv_protocol::Noise::obfs() };
        p.connect_guest(link).await.expect("гость не пожал руку")
    }

    /// СЛОТ ГОСТЯ ОБЯЗАН ОСВОБОЖДАТЬСЯ ПРИ ОТМЕНЕ ЗАДАЧИ. Учёт снимался только на
    /// нормальном возврате, а сессии гостей во ВСЕХ оболочках живут в
    /// `tokio::spawn`: отмена (или паника) оставляла запись навсегда. Счётчик
    /// полз вверх, и после `max_guests` таких событий хост отказывал ВСЕМ, честно
    /// показывая «X из X» при нуле реальных гостей.
    #[tokio::test]
    async fn cancelled_session_frees_the_guest_slot() {
        let eng = offline_engine(Config::default());
        let (host_side, guest_link) = bmv_common::wire::memory_pair(16);
        let peer: SocketAddr = "203.0.113.7:41000".parse().unwrap();

        let proto = eng.host_protocol.lock().clone();
        let guest = tokio::spawn(async move {
            let link = guest_side(guest_link, &proto).await;
            tokio::time::sleep(Duration::from_secs(10)).await; // держим канал живым
            drop(link);
        });
        let e2 = eng.clone();
        let session = tokio::spawn(async move { e2.host_run_session(peer, host_side, false).await });

        // Ждём, пока гость встанет на учёт.
        for _ in 0..200 {
            if !eng.active_guests.lock().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(eng.active_guests.lock().len(), 1, "гость не попал в учёт — тест ничего не проверяет");

        // Так шелл гасит сессию: задача отменяется, кода после await не будет.
        session.abort();
        let _ = session.await;
        assert!(
            eng.active_guests.lock().is_empty(),
            "после отмены задачи слот гостя остался занят — счётчик ползёт вверх навсегда"
        );
        guest.abort();
    }

    /// ШТРАФНАЯ ПАУЗА НЕ ДОЛЖНА ДЕРЖАТЬ ОБЩИЕ ВОРОТА. Пауза на неверном пароле —
    /// заявленная защита от перебора, но пока она занимала пермит `handshake_gate`,
    /// 64 неверных пароля в секунду выкупали ВСЕ ворота, и легальные гости не
    /// подключались вовсе: защита от перебора превращалась в готовый DoS.
    #[tokio::test]
    async fn wrong_password_penalty_does_not_hold_the_handshake_gate() {
        let mut cfg = Config::default();
        cfg.host.password = "правильный".into();
        let eng = offline_engine(cfg);
        let (host_side, guest_link) = bmv_common::wire::memory_pair(16);

        let proto = eng.host_protocol.lock().clone();
        let guest = tokio::spawn(async move {
            let link = guest_side(guest_link, &proto).await;
            link.send(&auth_frame("НЕ тот")).await.unwrap();
            tokio::time::sleep(Duration::from_secs(10)).await;
        });
        let e2 = eng.clone();
        let peer: SocketAddr = "203.0.113.9:41001".parse().unwrap();
        let session = tokio::spawn(async move { e2.host_run_session(peer, host_side, false).await });

        // Пауза — секунда; смотрим в её середину.
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(!session.is_finished(), "сессия уже завершилась — момент паузы не пойман");
        assert_eq!(
            eng.handshake_gate.available_permits(),
            HANDSHAKE_CONCURRENCY,
            "штрафная пауза держит общие ворота — перебором паролей выключается весь хост"
        );
        let _ = session.await;
        guest.abort();
    }

    /// КАК УЗЕЛ НАЗЫВАЕТ СЕБЯ — ОДИНАКОВО У ХОСТА И У ГОСТЯ. Список писался
    /// дважды, и копии разошлись: провал STUN у хоста попадал в журнал, у гостя
    /// исчезал молча. Хвост без внешнего адреса означает «из интернета меня не
    /// достать» для обоих одинаково.
    #[test]
    fn a_node_names_itself_the_same_way_on_both_sides() {
        let ext: SocketAddr = "45.11.22.33:40000".parse().unwrap();
        let with = my_endpoints(40000, Some(ext));
        assert!(with.contains(&ext.to_string()), "внешний адрес обязан попасть в список: {with:?}");

        // Без STUN список НЕ пустеет: домашний адрес остаётся, пир из той же
        // локальной сети достучится.
        let without = my_endpoints(40000, None);
        assert!(!without.contains(&ext.to_string()));
        assert_eq!(with.len(), without.len() + 1, "провал STUN обязан стоить ровно один адрес");

        // ПОВТОР ОБЯЗАН СХЛОПЫВАТЬСЯ. Узел без NAT (машина с белым адресом)
        // видит из STUN РОВНО СВОЙ адрес — и тогда он называл бы себя дважды.
        // Каждый лишний адрес в списке стоит второй стороне 12 секунд UDP-пачек
        // в никуда, а хост получает их по числу гостей.
        if let Some(ip) = bmv_net::local_ip() {
            let same: SocketAddr = format!("{ip}:40000").parse().unwrap();
            assert_eq!(my_endpoints(40000, Some(same)), vec![same.to_string()], "свой же адрес назван дважды");
        }
    }

    /// ЦЕЛИ ПРОБИВАНИЯ — НЕ ПО ЧУЖИМ СЛОВАМ. Адреса приходят от гостя (и от хоста
    /// для гостя), а по каждому уходит 48 пачек UDP. Внутренние адреса обязаны
    /// отсеиваться, дубликаты — схлопываться, число целей — быть ограничено.
    #[test]
    fn parse_addrs_drops_internal_targets_and_duplicates() {
        let list: Vec<String> = [
            "127.0.0.1:41000",        // петля — сам себе сканер
            "169.254.169.254:80",     // метаданные облака
            "[::1]:41000",
            "224.0.0.1:41000",        // мультикаст
            "45.11.22.33:40000",      // нормальный публичный
            "45.11.22.33:40000",      // ...и его дубль
            "192.168.1.5:40000",      // гость в той же LAN — ОСТАВЛЯЕМ
            "не адрес",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let got = parse_addrs(&list);
        let s: Vec<String> = got.iter().map(|a| a.to_string()).collect();
        assert!(!s.iter().any(|a| a.starts_with("127.") || a.starts_with("[::1]")), "петля в целях: {s:?}");
        assert!(!s.iter().any(|a| a.starts_with("169.254.")), "link-local/метаданные в целях: {s:?}");
        assert!(!s.iter().any(|a| a.starts_with("224.")), "мультикаст в целях: {s:?}");
        assert_eq!(s.iter().filter(|a| a.starts_with("45.11.22.33")).count(), 1, "дубликаты не схлопнуты: {s:?}");
        assert!(s.iter().any(|a| a.starts_with("192.168.1.5")), "гость из своей LAN отрезан: {s:?}");

        // Длинный список от недоброго координатора не должен превращаться в веер.
        let many: Vec<String> = (1..100).map(|i| format!("45.11.22.{i}:40000")).collect();
        assert!(parse_addrs(&many).len() <= MAX_PUNCH_TARGETS, "число целей не ограничено");
    }

    /// ОДИН ДЕФОЛТНЫЙ ПРОТОКОЛ НА ВЕСЬ ПРОЕКТ. Пустая строка из оболочки давала
    /// гостю «noise», а хосту — «noise-obfs»: рукопожатие шло не с тем протоколом
    /// и выглядело как 12 секунд тишины вместо понятной ошибки.
    #[test]
    fn default_protocol_is_one_for_everyone() {
        assert_eq!(bmv_config::DEFAULT_PROTOCOL, Config::default().default_protocol);

        // Пустая настройка из оболочки → тот же дефолт, а не «первый доступный».
        let cfg = Config { default_protocol: String::new(), ..Default::default() };
        let eng = BmvEngine::from_config(cfg);
        assert_eq!(*eng.host_protocol.lock(), bmv_config::DEFAULT_PROTOCOL);
        assert_eq!(eng.protocol_by_name("").name(), bmv_config::DEFAULT_PROTOCOL,
            "гость без явного протокола обязан брать общий дефолт");

        // Неизвестное имя не подменяется молча «первым доступным».
        assert_eq!(eng.protocol_by_name("выдуманный").name(), bmv_config::DEFAULT_PROTOCOL);
        assert_eq!(eng.protocol_by_name("plain").name(), "plain", "известное имя обязано работать");
    }

    /// ЭКРАН ГОВОРИТ ТО, ЧЕМ ИДЁТ СЕССИЯ.
    ///
    /// На один вопрос «каким протоколом мы работаем» в этом файле было ТРИ
    /// ответа: сессия разбирала имя через `protocol_by_name`, `active_protocol`
    /// брала «первый доступный из реестра», а демо-проверка — «протокол из
    /// настройки, иначе plain». При пустой настройке (её кладёт оболочка, у
    /// которой поле протокола не заполнено) это давало: сессия — «noise-obfs»,
    /// экран — «noise», проверка связи — вообще БЕЗ ШИФРА. Здесь проверяется,
    /// что ответ ровно один, на всех четырёх видах настройки.
    #[test]
    fn the_screen_names_the_protocol_the_session_uses() {
        for setting in ["", "выдуманный", "plain", bmv_config::DEFAULT_PROTOCOL] {
            let eng = BmvEngine::from_config(Config {
                default_protocol: setting.to_string(),
                ..Default::default()
            });
            // Чем ПОЙДЁТ сессия: host_run_session разбирает ровно это имя.
            let real = eng.protocol_by_name(&eng.host_protocol.lock().clone()).name();
            assert_eq!(
                eng.active_protocol(), real,
                "настройка {setting:?}: человеку показываем «{}», а работаем «{real}»",
                eng.active_protocol()
            );
            // Витрина протоколов начинается с того же самого.
            assert_eq!(
                eng.connect_order().first().map(|p| p.name()),
                Some(real),
                "настройка {setting:?}: список протоколов начинается не с рабочего"
            );
        }
    }

    /// НАСТРОЙКА STUN ДОЕЗЖАЕТ ДО МЕСТА РЕШЕНИЯ РАЗОБРАННОЙ, А НЕ ФАЙЛОМ.
    ///
    /// `StunConfig::resolve` читает файл с диска, и её звали из пяти мест — в том
    /// числе из тика хоста, то есть блокирующее чтение файла раз в десять секунд
    /// из async-задачи, всё время раздачи. На телефонах файла нет вовсе, там это
    /// был отказ open() каждые десять секунд навсегда.
    #[test]
    fn stun_servers_are_parsed_once_when_the_engine_is_built() {
        let dir = std::env::temp_dir().join(format!(
            "bmv-stun-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("stun_servers.txt");
        std::fs::write(&path, "первый.example:3478\n# коммент\nвторой.example:3478\n").unwrap();

        let mut cfg = Config::default();
        cfg.stun.file = path.display().to_string();
        let eng = BmvEngine::from_config(cfg);

        // Файл убрали — движок обязан помнить то, что прочитал при сборке.
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            eng.stun.as_ref(),
            ["первый.example:3478".to_string(), "второй.example:3478".to_string()].as_slice(),
            "список STUN выводится из файловой системы на каждом обращении, а не из настройки"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Пароль ⇒ сеть скрытая, что бы ни стояло в настройке публичности.
    /// Правило живёт в build_announce, потому что через неё проходят ВСЕ пути
    /// (старт, смена пароля на лету, восстановление после реконнекта) и все
    /// платформы — иначе одна из оболочек рано или поздно разошлась бы с ядром.
    #[test]
    fn password_forces_hidden_network() {
        let mut cfg = bmv_config::Config::default();
        cfg.host.public = true;
        cfg.host.password = "секрет".into();
        let eng = BmvEngine::from_config(cfg);
        let ann = eng.build_announce();
        assert!(ann.has_password, "пароль должен быть виден как флаг");
        assert!(!ann.public, "с паролем сеть обязана быть скрытой");

        // Без пароля публичность работает как обычно.
        let mut cfg2 = bmv_config::Config::default();
        cfg2.host.public = true;
        let eng2 = BmvEngine::from_config(cfg2);
        assert!(eng2.build_announce().public, "без пароля публичный хост остаётся публичным");
    }
}
