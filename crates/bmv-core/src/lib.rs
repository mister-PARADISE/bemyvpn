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
}

/// Потолок одновременных рукопожатий гостей (анти-флуд, см. handshake_gate).
const HANDSHAKE_CONCURRENCY: usize = 64;

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
            "noise".to_string()
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
            config,
            protocols: Arc::new(Registry::with_builtins()),
            host_id,
            host_token,
            host_endpoints: Arc::new(Mutex::new(Vec::new())),
            active_guests: Arc::new(Mutex::new(HashMap::new())),
            session_gen: Arc::new(AtomicU64::new(0)),
            handshake_gate: Arc::new(tokio::sync::Semaphore::new(HANDSHAKE_CONCURRENCY)),
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn host_id(&self) -> &str {
        &self.host_id
    }

    /// Автоопределение типа NAT этого узла: "cone" (мягкий — прямой прострел
    /// возможен), "symmetric" (строгий — внешний порт зависит от адресата) или ""
    /// (не удалось). Для UI/диагностики и раннего вывода «нужен релей». Занимает
    /// пару секунд (STUN на два сервера) — звать из фонового потока, не на UI.
    pub async fn classify_nat(&self) -> String {
        let servers = self.config.stun.resolve();
        bmv_net::classify_mapping(&servers, Duration::from_secs(4)).await
    }

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
            .ok_or_else(|| bmv_common::Error::Config("в конфиге не задан координатор".into()))?;
        let fresh = bmv_signal::Coordinator::new(base)?;
        // Гонка двух первых вызовов безопасна: проигравший Coordinator дропается,
        // его супервизор ещё не стартовал (стартует лишь при первом использовании).
        let _ = self.coord.set(fresh);
        Ok(self.coord.get().expect("только что установлен").clone())
    }

    /// Проверить, что координатор жив.
    pub async fn coordinator_health(&self) -> Result<()> {
        self.coordinator()?.health().await
    }

    /// ГОСТЬ: получить список хостов из каталога.
    pub async fn guest_list(
        &self,
        country: Option<String>,
        public_only: bool,
    ) -> Result<Vec<bmv_signal::HostInfo>> {
        let coord = self.coordinator()?;
        coord
            .directory(&bmv_signal::Filter {
                country,
                public_only,
            })
            .await
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
        public_only: bool,
        since: u64,
    ) -> Result<bmv_signal::DirectoryUpdate> {
        let coord = self.coordinator()?;
        coord
            .directory_watch(
                &bmv_signal::Filter {
                    country,
                    public_only,
                },
                since,
            )
            .await
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
        let mut candidates: Vec<String> = Vec::new();
        if let Some(ip) = bmv_net::local_ip() {
            candidates.push(format!("{ip}:{port}"));
        }
        let servers = self.config.stun.resolve();
        if let Ok(ext) = ep.reflexive(&servers, Duration::from_secs(3)).await {
            candidates.push(ext.to_string());
        }
        candidates.sort();
        candidates.dedup();
        log::info!("ГОСТЬ: мои кандидаты для хоста: {:?}", candidates);

        let resp = coord
            .connect(&bmv_signal::GuestConnect {
                host_id: host_id.to_string(),
                candidates,
            })
            .await?;
        let peers = parse_addrs(&resp.host_endpoints);
        log::info!("ГОСТЬ: адреса хоста от координатора: {:?}", resp.host_endpoints);
        if peers.is_empty() {
            return Err(bmv_common::Error::Net("у хоста нет валидных адресов".into()));
        }

        // Пробиваем NAT к хосту и поднимаем протокол (гость — инициатор).
        // Токены пробивания выводятся из host_id (не общий ASCII-маркер).
        // Окно щедрое: между двумя мобильными NAT пробитие может занять секунды.
        let (pt, pk) = bmv_net::punch_tokens(host_id);
        let (peer, primed) = ep.hole_punch(&peers, Duration::from_secs(12), &pt, &pk).await?;
        // Протокол ДОЛЖЕН совпадать с хостом (гость берёт его из каталога).
        let proto = self.protocol_by_name(protocol.unwrap_or("noise"));
        let link = ep.connect_primed(peer, primed, pt, pk);
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

    /// ХОСТ: поднять хаб-сокет (мультигость), собрать кандидаты, анонсировать.
    /// Возвращает хаб (держать!), id и список анонсированных адресов.
    pub async fn host_bind_announce(
        &self,
    ) -> Result<(std::sync::Arc<bmv_net::UdpHub>, String, Vec<String>)> {
        // STUN делаем ВНУТРИ bind (до старта демультиплексора) — иначе demux
        // съедает STUN-ответ и хост за NAT узнаёт только свой LAN-адрес.
        let servers = self.config.stun.resolve();
        // Токены встречного пробивания — из СВОЕГО host_id (гость выведет те же).
        let (pt, pk) = bmv_net::punch_tokens(&self.host_id);
        let (hub, reflexive) =
            bmv_net::UdpHub::bind_reflexive("0.0.0.0:0".parse().unwrap(), &servers, Duration::from_secs(4), pt, pk)
                .await?;
        let port = hub.local_addr()?.port();

        let mut endpoints: Vec<String> = Vec::new();
        if let Some(ip) = bmv_net::local_ip() {
            endpoints.push(format!("{ip}:{port}"));
        }
        match reflexive {
            Some(ext) => endpoints.push(ext.to_string()),
            None => tracing::warn!("STUN не удался, анонсирую без внешнего адреса"),
        }
        endpoints.sort();
        endpoints.dedup();

        // Запоминаем адреса — по ним пойдут все ре-анонсы (heartbeat + при
        // изменении числа гостей).
        *self.host_endpoints.lock() = endpoints.clone();
        self.announce_state().await?;
        Ok((hub, self.host_id.clone(), endpoints))
    }

    /// Собрать анонс из конфига + ЖИВОГО числа гостей + рантайм-настроек.
    fn build_announce(&self) -> bmv_signal::HostAnnounce {
        bmv_signal::HostAnnounce {
            id: self.host_id.clone(),
            token: self.host_token.clone(),
            name: self.host_name.lock().clone(),
            endpoints: self.host_endpoints.lock().clone(),
            country: self.config.host.country_hint.clone(),
            public: self.host_public.load(Ordering::SeqCst),
            max_guests: self.host_max.load(Ordering::SeqCst),
            guests: self.active_guests.lock().len() as u32,
            has_password: !self.host_password.lock().is_empty(),
            protocol: self.host_protocol.lock().clone(),
            code_sig: self.host_code_sig.clone(),
            params: bmv_signal::Params::new(),
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

    /// Выбрать протокол по имени (или первый доступный как фолбэк).
    fn protocol_by_name(&self, name: &str) -> Arc<dyn Protocol> {
        self.protocols
            .get(name)
            .or_else(|| self.connect_order().into_iter().next())
            .expect("хотя бы один протокол есть")
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
        // ПОВТОРНАЯ проверка ПЕРЕД самой отправкой. Между первой проверкой и этим
        // местом мог пройти host_deannounce (он ставит host_active=false, ЗАТЕМ
        // шлёт bye). Если мы всё ещё видим true — значит deannounce ещё не начался,
        // и наш announce гарантированно уходит РАНЬШЕ bye (WS упорядочен) → хост
        // не «воскресает» после снятия. Без этого утёкший/выходящий анонс возвращал
        // снятый хост в каталог.
        if !self.host_active.load(Ordering::SeqCst) {
            return Ok(());
        }
        // Живость хоста теперь — сам WS-сокет к координатору (закрылся → убрали
        // мгновенно). Отдельная UDP-проверка больше не нужна.
        coord.announce(&ann).await?;
        Ok(())
    }

    /// Совместимость: ре-анонс с явными адресами (обновляет и сохранённый набор).
    pub async fn host_reannounce(&self, endpoints: &[String]) -> Result<()> {
        *self.host_endpoints.lock() = endpoints.to_vec();
        self.announce_state().await
    }

    /// ХОСТ: периодический тик — держит NAT-дырку hub-сокета открытой (иначе
    /// хост за NAT становится недостижим для новых гостей) и обновляет запись в
    /// каталоге. Звать раз в ~15с из цикла приёма гостей.
    pub async fn host_heartbeat(&self, hub: &bmv_net::UdpHub) -> Result<()> {
        hub.nat_keepalive(&self.config.stun.resolve()).await;
        self.announce_state().await
    }

    /// ХОСТ: цикл ВСТРЕЧНОГО ПРОБИТИЯ. Long-poll'ит у координатора ждущих гостей
    /// и шлёт им PUNCH, открывая свой NAT навстречу (иначе хост за NAT недостижим).
    /// Крутить фоном рядом с циклом приёма гостей.
    pub async fn host_serve_punch(&self, hub: std::sync::Arc<bmv_net::UdpHub>) -> Result<()> {
        let coord = self.coordinator()?;
        let mut since = 0u64;
        loop {
            let p = match coord.pending(&self.host_id, since).await {
                Ok(p) => p,
                Err(_) => {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
            };
            since = p.version;
            for cand_set in p.guests {
                let addrs = parse_addrs(&cand_set);
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
    }

    /// ХОСТ: убрать себя из каталога СРАЗУ и НАВСЕГДА заглушить этот движок
    /// (host_active=false) — чтобы никакая утёкшая/выходящая задача его не
    /// воскресила. Zовём при остановке раздачи.
    pub async fn host_deannounce(&self) -> Result<()> {
        self.host_active.store(false, Ordering::SeqCst);
        self.coordinator()?.bye(&self.host_id, &self.host_token).await
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
            let _permit = self.handshake_gate.acquire().await.map_err(|_| bmv_common::Error::other("gate закрыт"))?;
            proto.connect_host(raw).await? // рукопожатие ДО учёта
        };
        let link: Box<dyn Link> = Box::new(bmv_common::KeepaliveLink::new(plink));

        // ПАРОЛЬ: если задан — первый пакет гостя должен быть верным auth-кадром.
        // Неверный/отсутствует → вежливо закрываем, слот не занимаем.
        let pw = self.host_password.lock().clone();
        if !pw.is_empty() {
            let ok = matches!(
                tokio::time::timeout(Duration::from_secs(6), link.recv()).await,
                Ok(Ok(frame)) if verify_auth(&frame, &pw)
            );
            if !ok {
                let _ = link.close().await;
                return Err(bmv_common::Error::Protocol("неверный пароль".into()));
            }
        }

        // Учёт по адресу пира: с одного ip:port один гость; переподключение
        // заменяет запись. Своё поколение — чтобы протухшая сессия не сняла чужой.
        // Гард мьютекса НЕ держим через await (иначе future не Send).
        let gen = self.session_gen.fetch_add(1, Ordering::SeqCst);
        let max = self.host_max.load(Ordering::SeqCst);
        let full = {
            let mut g = self.active_guests.lock();
            if max > 0 && !g.contains_key(&peer) && g.len() as u32 >= max {
                true
            } else {
                g.insert(peer, gen);
                false
            }
        };
        if full {
            let _ = link.close().await;
            return Err(bmv_common::Error::Net("хост заполнен: лимит гостей".into()));
        }
        let _ = self.announce_state().await; // каталог видит нового гостя сразу

        let res = if tunnel {
            bmv_tunnel::run_host(link).await
        } else {
            echo_session(link).await
        };

        {
            // снимаем ТОЛЬКО если наше место не занял новый коннект (по поколению)
            let mut g = self.active_guests.lock();
            if g.get(&peer) == Some(&gen) {
                g.remove(&peer);
            }
        }
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

    /// Порядок протоколов для попытки соединения: выбранный + внутренний фолбэк.
    pub fn connect_order(&self) -> Vec<Arc<dyn Protocol>> {
        self.protocols
            .fallback_order(&self.config.default_protocol)
    }

    /// Имя протокола, который реально будет использован (первый доступный из
    /// порядка фолбэка) — для честного вывода в UI.
    pub fn active_protocol(&self) -> &'static str {
        self.connect_order()
            .first()
            .map(|p| p.name())
            .unwrap_or("—")
    }

    /// Узнать свой внешний адрес через STUN-пул (из файла/конфига или встроенный).
    pub async fn external_addr(&self) -> Result<SocketAddr> {
        let servers = self.config.stun.resolve();
        bmv_net::reflexive_addr(&servers, Duration::from_secs(5)).await
    }

    /// ДЕМО: прогнать пакет host↔guest через выбранный протокол по in-memory
    /// каналу (loopback, без сети). Доказывает связку конфиг → реестр → протокол.
    pub async fn demo_loopback(&self, payload: &[u8]) -> Result<Vec<u8>> {
        use bmv_common::wire::memory_pair;

        let proto = self
            .protocols
            .get(&self.config.default_protocol)
            .or_else(|| self.protocols.get("plain"))
            .ok_or_else(|| bmv_common::Error::Protocol("нет ни одного протокола".into()))?;

        tracing::info!(protocol = proto.name(), "демо loopback");

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

/// Разобрать список строк "ip:port" в адреса, пропуская неразборчивые.
fn parse_addrs(list: &[String]) -> Vec<SocketAddr> {
    list.iter().filter_map(|s| s.parse().ok()).collect()
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
async fn echo_session(link: Box<dyn Link>) -> Result<()> {
    loop {
        let pkt = link.recv().await?;
        if pkt.is_empty() {
            return Ok(());
        }
        link.send(&pkt).await?;
    }
}
