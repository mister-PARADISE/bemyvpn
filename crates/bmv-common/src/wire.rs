//! «Провод» между двумя пирами — датаграммный двусторонний канал.
//!
//! Почему датаграммы, а не байтовый поток: VPN несёт IP-ПАКЕТЫ. Пакет — это
//! естественная единица. `Link` шлёт и принимает пакеты целиком; кадрирование
//! (если транспорт потоковый) — забота конкретной реализации.
//!
//! Слои стыкуются так: `bmv-net` даёт сырой `Link` (UDP после hole-punch);
//! `bmv-protocol` оборачивает его в шифрованный `Link`; `bmv-tunnel` качает
//! через него IP-пакеты.

use async_trait::async_trait;

use crate::Result;

/// Готовый двусторонний канал передачи пакетов между пирами.
///
/// Методы берут `&self` (внутренняя синхронизация в реализациях), чтобы можно
/// было одновременно `send` и `recv` из разных задач — это нужно туннелю,
/// который качает пакеты в обе стороны параллельно. `Sync` — чтобы делить через `Arc`.
///
/// ГОРЯЧИЙ ПУТЬ БЕЗ АЛЛОКАЦИЙ: основной приём — `recv_into(buf)`, он ПЕРЕЗАПИСЫВАЕТ
/// переданный `buf` одним пакетом (вызывающий переиспользует буфер → нет malloc на
/// каждый пакет; критично для CPU/батареи телефона при высоком pps). `recv()` —
/// удобная обёртка (аллоцирует) для редких «сигнальных» вызовов.
#[async_trait]
pub trait Link: Send + Sync {
    async fn send(&self, packet: &[u8]) -> Result<()>;

    /// Принять ОДИН пакет в `buf` (буфер очищается и заполняется). `Ok(true)` —
    /// пакет получен; `Ok(false)` — канал закрыт (EOF). Вызывающий владеет `buf`
    /// и переиспользует его между вызовами.
    async fn recv_into(&self, buf: &mut Vec<u8>) -> Result<bool>;

    /// Удобная аллоцирующая обёртка над `recv_into`. Пустой `Vec` = канал закрыт.
    /// Для горячего пути используйте `recv_into` с переиспользуемым буфером.
    async fn recv(&self) -> Result<Vec<u8>> {
        let mut buf = Vec::new();
        if self.recv_into(&mut buf).await? {
            Ok(buf)
        } else {
            Ok(Vec::new())
        }
    }

    async fn close(&self) -> Result<()> {
        Ok(())
    }

    /// Канал закрылся потому, что пир ПОПРОЩАЛСЯ (BYE), а не потому, что пропал?
    ///
    /// Для верхнего слоя это разные события, а `recv_into` возвращает на оба одно
    /// и то же `Ok(false)`. Разница дорогая: после обрыва пути канал надо
    /// ПЕРЕУСТАНАВЛИВАТЬ (мобильный роуминг — обычное дело), а после прощания
    /// переустанавливать НЕКУДА — хост погасил раздачу, и попытки реконнекта
    /// оборачиваются двумя минутами «подключаюсь» вместо честного «отключено».
    fn peer_said_bye(&self) -> bool {
        false
    }

    /// ПОДСКАЗКА (не приказ!) «проверь соседа немедленно».
    ///
    /// Зовётся, когда координатор сообщил, что пир от НЕГО отвалился: у
    /// координатора сокет по TCP, и он узнаёт об уходе даже там, где послать
    /// прощание физически невозможно (приложение убили, сеть исчезла).
    ///
    /// СОЗНАТЕЛЬНО НЕ РВЁТ СЕССИЮ. Слово координатора не может быть командой
    /// разрыва: иначе он (и всякий, кто сумеет к нему подделаться) получил бы
    /// кнопку удалённого отключения чужого туннеля — при том что весь смысл
    /// продукта в том, что трафик через него не идёт. Здесь подсказка лишь
    /// ужимает срок тишины и опрашивает пира: живой ответит и ничего не заметит,
    /// мёртвый промолчит и будет отключён на несколько секунд раньше.
    fn check_peer_now(&self) {}
}

// ── Keepalive: детект обрыва поверх любого Link ──────────────────────────────

/// Маркер живости — один нулевой байт. Настоящий IP-пакет так не выглядит
/// (минимум 20 байт, версия 4/6 в старшем ниббле), поэтому не спутать с данными.
const KEEPALIVE: [u8; 1] = [0u8];
/// Маркер прощания: сторона закрывает канал СОЗНАТЕЛЬНО. Другая сторона видит
/// EOF мгновенно, не дожидаясь keepalive-таймаута (важно для живого счётчика
/// гостей: отключился — каталог узнаёт сразу).
const BYE: [u8; 1] = [1u8];
/// «Ты ещё там?» — АКТИВНЫЙ опрос пира по подсказке координатора
/// (`Link::check_peer_now`). Получатель обязан немедленно ответить `KEEPALIVE`;
/// это подтверждение живости, не зависящее от расписания пингов.
///
/// Старые сборки маркера не знают и отдадут его наверх как данные — там
/// однобайтовый «IP-пакет» отбрасывается (у гостя `from_host_allowed`, у хоста
/// `ipstack` как UnknownNetwork). То есть ответа не будет, и решение примет
/// короткий срок тишины — ровно так же, как если бы опроса не было вовсе.
const PROBE: [u8; 1] = [2u8];
/// Джиттер интервала keepalive. Фиксированные ровно 3.000с в обе стороны были
/// периодическим маячком: обфускация прячет РАЗМЕР пакета, но не ПЕРИОД — по нему
/// DPI фингерпринтит туннель на простое. Случайные интервалы ломают периодичность.
/// Учащены (было 2–5с): быстрее детект обрыва (8с вместо 10с) И свежее держится
/// NAT-дырка — на мобильном (смена вышек) мэппинг реже протухает до реконнекта.
const PING_MIN_MS: u64 = 1500;
const PING_MAX_MS: u64 = 3500;
/// Порог «пир мёртв». > 2× максимального интервала пинга (3.5с → 7с), чтобы ОДИН
/// потерянный keepalive не вызвал ложный обрыв, но реальный обрыв ловится за 8с
/// (было 10с). Меньше делать нельзя — джиттер+потеря дадут ложные разрывы на
/// мобильном, а это хуже редкого лишнего реконнекта.
const DEAD_AFTER: std::time::Duration = std::time::Duration::from_secs(8);
/// Сколько раз повторяем прощание и с каким шагом.
///
/// BYE ненадёжен по природе: один UDP-пакет без подтверждения. Прежняя пара
/// `send` ПОДРЯД шансов почти не добавляла — два пакета, ушедшие в одну и ту же
/// микросекунду, идут одним всплеском и теряются ВМЕСТЕ (полисинг по pps,
/// переполнение очереди на пути, потеря на смене вышки). Разнесённые во времени
/// повторы — три независимые попытки вместо одной.
const BYE_REPEATS: usize = 3;
const BYE_GAP: std::time::Duration = std::time::Duration::from_millis(150);
/// Срок тишины ПОСЛЕ подсказки координатора «проверь соседа».
///
/// Подсказка — это НЕ приказ рвать сессию (иначе координатор и всякий, кто сумеет
/// к нему подделаться, получил бы кнопку удалённого отключения чужого туннеля, а
/// весь смысл проекта в том, что трафик через него не идёт). Поэтому по подсказке
/// мы лишь ужимаем срок тишины и опрашиваем пира.
///
/// 4 секунды — НИЖНИЙ безопасный предел, а не «поменьше бы»: живой пир обязан
/// успеть доказать, что он жив, СВОИМИ силами. Доказательств два, и они
/// независимы: ответ на `PROBE` (одно RTT, но его не знают старые сборки) и
/// собственный keepalive пира (не реже `PING_MAX_MS` = 3.5с в ЛЮБОЙ версии).
/// Меньше 3.5с — и подсказка начала бы убивать живые сессии старых сборок, а
/// ложный разрыв хуже, чем разрыв, замеченный на пару секунд позже.
const PEER_CHECK_GRACE: std::time::Duration = std::time::Duration::from_secs(4);

/// Обёртка `Link`, дающая детект обрыва (UDP сам разрыв не сигналит).
///
/// Фоном шлёт пирy keepalive-пустышки; в `recv` фильтрует их и обновляет
/// «время последней активности». Если от пира тишина дольше `DEAD_AFTER` —
/// `recv` возвращает пустой `Vec` (EOF), и туннель штатно завершает сессию.
pub struct KeepaliveLink {
    inner: std::sync::Arc<dyn Link>,
    /// Двойное назначение: глушит пингер И делает прощание ОДНОРАЗОВЫМ (кто
    /// первым взвёл — тот и прощается; `close()` и `Drop` не дублируют друг друга).
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Пир ПРИСЛАЛ BYE, а не просто замолчал (см. `Link::peer_said_bye`).
    peer_bye: std::sync::atomic::AtomicBool,
    /// Подсказка «проверь соседа немедленно» (см. `Link::check_peer_now`).
    /// `notify_one`, а не `notify_waiters`: подсказка, пришедшая в щель между
    /// двумя ожиданиями, не теряется — её заберёт следующий `recv_into`.
    probe: tokio::sync::Notify,
}

/// НАСТОЙЧИВОЕ прощание в фоне: `BYE_REPEATS` пакетов с интервалом `BYE_GAP`.
///
/// Задача держит `inner` живым на время повторов — значит сокет не умрёт раньше,
/// чем прощание уйдёт (раньше `Drop` канала закрывал путь мгновенно). Вне
/// рантайма (дроп из синхронного кода) молча ничего не делаем: BYE — вежливость,
/// а не транзакция, приёмник обязан узнать о разрыве и по тишине (`DEAD_AFTER`).
fn farewell(inner: std::sync::Arc<dyn Link>) {
    let Ok(rt) = tokio::runtime::Handle::try_current() else { return };
    rt.spawn(async move {
        for i in 0..BYE_REPEATS {
            if i > 0 {
                tokio::time::sleep(BYE_GAP).await;
            }
            if inner.send(&BYE).await.is_err() {
                break; // путь уже мёртв — повторять некуда
            }
        }
    });
}

impl KeepaliveLink {
    pub fn new(inner: Box<dyn Link>) -> Self {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let inner: Arc<dyn Link> = Arc::from(inner);
        let stop = Arc::new(AtomicBool::new(false));
        let pinger = inner.clone();
        let pstop = stop.clone();
        tokio::spawn(async move {
            use rand::Rng;
            loop {
                // Спим СНАЧАЛА (первый пинг не мгновенный) на случайный интервал.
                let ms = rand::thread_rng().gen_range(PING_MIN_MS..=PING_MAX_MS);
                tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                if pstop.load(Ordering::Relaxed) || pinger.send(&KEEPALIVE).await.is_err() {
                    break;
                }
            }
        });
        KeepaliveLink {
            inner,
            stop,
            peer_bye: AtomicBool::new(false),
            probe: tokio::sync::Notify::new(),
        }
    }
}

/// ПРОЩАНИЕ ЖИВЁТ В `Drop`, А НЕ ТОЛЬКО В `close()`.
///
/// `close()` зовут лишь пути ШТАТНОГО возврата, а гасят сессии во всех четырёх
/// оболочках ОТМЕНОЙ задачи: раздача — дропом `JoinSet` (`bmv-desktop::hosting`,
/// хост-режим в `bmv-ffi`), гость — abort'ом качалки. Отменённая задача не
/// исполняет код после `await`, поэтому `link.close()` в конце `run_host`/
/// `run_guest` просто не наступал — и хост, погасивший раздачу, НЕ прощался ни с
/// одним гостем: те висели «подключены» до keepalive-таймаута. Отсюда и
/// «уведомление срабатывает далеко не всегда». `Drop` же исполняется на ЛЮБОМ
/// выходе — включая отмену и панику, — поэтому прощание переехало сюда.
impl Drop for KeepaliveLink {
    fn drop(&mut self) {
        if !self.stop.swap(true, std::sync::atomic::Ordering::Relaxed) {
            farewell(self.inner.clone());
        }
    }
}

#[async_trait]
impl Link for KeepaliveLink {
    async fn send(&self, packet: &[u8]) -> Result<()> {
        self.inner.send(packet).await
    }

    async fn recv_into(&self, buf: &mut Vec<u8>) -> Result<bool> {
        // Срок тишины на ОДНО ожидание: обычно `DEAD_AFTER`, а после подсказки
        // координатора — короткий `PEER_CHECK_GRACE`. Любой пакет от пира
        // возвращает обычный срок: пир доказал, что жив.
        let mut wait = DEAD_AFTER;
        let mut ask_peer = false;
        loop {
            if std::mem::take(&mut ask_peer) {
                // Спрашиваем напрямую. Ответа может и не быть (на том конце старая
                // сборка) — тогда живость подтвердит собственный keepalive пира.
                let _ = self.inner.send(&PROBE).await;
            }
            let got = tokio::select! {
                r = tokio::time::timeout(wait, self.inner.recv_into(buf)) => r,
                // Подсказка «проверь соседа»: ожидание перезапускаем с коротким
                // сроком. Отмена `recv_into` безопасна — все реализации читают из
                // сокета/очереди, недочитанного состояния не бывает.
                _ = self.probe.notified() => {
                    wait = PEER_CHECK_GRACE;
                    ask_peer = true;
                    continue;
                }
            };
            match got {
                Ok(Ok(false)) => return Ok(false), // канал закрыт
                // Пустышка — глотаем; заодно это доказательство живости пира.
                Ok(Ok(true)) if buf.as_slice() == KEEPALIVE => {
                    wait = DEAD_AFTER;
                    continue;
                }
                // «Ты ещё там?» — отвечаем НЕМЕДЛЕННО: это и есть доказательство.
                Ok(Ok(true)) if buf.as_slice() == PROBE => {
                    let _ = self.inner.send(&KEEPALIVE).await;
                    wait = DEAD_AFTER;
                    continue;
                }
                // Пир СОЗНАТЕЛЬНО попрощался (чистый выход) — EOF мгновенно.
                Ok(Ok(true)) if buf.as_slice() == BYE => {
                    self.peer_bye.store(true, std::sync::atomic::Ordering::Relaxed);
                    return Ok(false);
                }
                Ok(Ok(true)) => return Ok(true),
                Ok(Err(e)) => return Err(e),
                // Тишина дольше срока → пир мёртв (резкий обрыв без BYE).
                Err(_) => return Ok(false),
            }
        }
    }

    async fn close(&self) -> Result<()> {
        // Первый BYE — СИНХРОННО: вызывающий (iOS `bmv_stop`) обязан знать, что
        // прощание уже на проводе, ПРЕЖДЕ чем платформа снесёт ресурсы туннеля.
        // Остальные — фоном, с интервалом (см. `farewell`).
        if !self.stop.swap(true, std::sync::atomic::Ordering::Relaxed) {
            let _ = self.inner.send(&BYE).await;
            farewell(self.inner.clone());
        }
        self.inner.close().await
    }

    fn peer_said_bye(&self) -> bool {
        self.peer_bye.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn check_peer_now(&self) {
        self.probe.notify_one();
    }
}

// ── In-memory Link для демо/тестов (loopback без сети) ───────────────────────

/// Пара связанных in-memory каналов (эмуляция канала между двумя пирами).
/// Используется в демо и юнит-тестах: то, что послали в A, читается из B.
pub fn memory_pair(capacity: usize) -> (Box<dyn Link>, Box<dyn Link>) {
    use tokio::sync::mpsc;
    let (tx_a, rx_b) = mpsc::channel::<Vec<u8>>(capacity);
    let (tx_b, rx_a) = mpsc::channel::<Vec<u8>>(capacity);
    (
        Box::new(MemLink {
            tx: tx_a,
            rx: tokio::sync::Mutex::new(rx_a),
        }),
        Box::new(MemLink {
            tx: tx_b,
            rx: tokio::sync::Mutex::new(rx_b),
        }),
    )
}

struct MemLink {
    tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    rx: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<Vec<u8>>>,
}

#[async_trait]
impl Link for MemLink {
    async fn send(&self, packet: &[u8]) -> Result<()> {
        self.tx
            .send(packet.to_vec())
            .await
            .map_err(|_| crate::Error::other("Связь оборвалась."))
    }

    async fn recv_into(&self, buf: &mut Vec<u8>) -> Result<bool> {
        let mut rx = self.rx.lock().await;
        match rx.recv().await {
            Some(pkt) => {
                buf.clear();
                buf.extend_from_slice(&pkt);
                Ok(true)
            }
            None => Ok(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// «Быстрый выход»: close() одной стороны шлёт BYE → другая видит EOF СРАЗУ,
    /// не дожидаясь keepalive-таймаута (DEAD_AFTER=8с). Это то, на чём держится
    /// «хост узнаёт об отключении гостя мгновенно».
    #[tokio::test]
    async fn bye_signals_eof_immediately_not_after_timeout() {
        let (a, b) = memory_pair(16);
        let ka = KeepaliveLink::new(a);
        let kb = KeepaliveLink::new(b);
        // Прощаемся с одной стороны.
        ka.close().await.unwrap();
        // Другая обязана увидеть EOF МНОГО быстрее DEAD_AFTER (8с). Даём 1с с запасом.
        let mut buf = Vec::new();
        let r = tokio::time::timeout(Duration::from_secs(1), kb.recv_into(&mut buf)).await;
        assert!(
            !r.expect("BYE не дал EOF за 1с — значит ждём keepalive-таймаут, выход НЕ мгновенный")
                .unwrap(),
            "после close()+BYE recv_into должен вернуть Ok(false) = EOF"
        );
    }

    /// ГЛАВНОЕ: прощание обязано уходить и при ДРОПЕ канала, а не только при
    /// `close()`. Все четыре оболочки гасят раздачу отменой задачи (дроп
    /// `JoinSet`), а отменённая задача до `link.close()` не доходит — то есть
    /// хост, выключивший раздачу, не прощался НИ С ОДНИМ гостем, и те висели
    /// «подключены» до keepalive-таймаута (8с).
    #[tokio::test]
    async fn dropping_the_link_says_goodbye_too() {
        let (a, b) = memory_pair(16);
        let ka = KeepaliveLink::new(a);
        let kb = KeepaliveLink::new(b);
        drop(ka); // ровно то, что делает отмена задачи: close() не звали

        let mut buf = Vec::new();
        let r = tokio::time::timeout(Duration::from_secs(1), kb.recv_into(&mut buf)).await;
        assert!(
            !r.expect("дроп канала не дал EOF за 1с — прощание живёт только в close(), а его на отмене задачи никто не зовёт")
                .unwrap(),
            "после дропа канала пир обязан увидеть EOF"
        );
        assert!(kb.peer_said_bye(), "EOF пришёл по тишине, а не по BYE — прощание не ушло");
    }

    /// Прощание обязано быть НАСТОЙЧИВЫМ: несколько попыток, РАЗНЕСЁННЫХ во
    /// времени. Два `send` подряд (как было) — это один всплеск: полисинг по pps
    /// или переполнение очереди на пути теряет оба разом, и уведомления нет.
    #[tokio::test]
    async fn farewell_is_repeated_and_spread_over_time() {
        // Пир БЕЗ keepalive-обёртки: считаем сырые BYE-пакеты, а не EOF.
        let (a, b) = memory_pair(64);
        let ka = KeepaliveLink::new(a);
        let started = tokio::time::Instant::now();
        ka.close().await.unwrap();

        let mut byes = 0usize;
        let mut last = started;
        while let Ok(Ok(pkt)) = tokio::time::timeout(Duration::from_secs(1), b.recv()).await {
            if pkt == BYE {
                byes += 1;
                last = tokio::time::Instant::now();
            }
            if byes >= BYE_REPEATS {
                break;
            }
        }
        assert!(byes >= BYE_REPEATS, "прощаний {byes}, а нужно не меньше {BYE_REPEATS}");
        assert!(
            last.duration_since(started) >= BYE_GAP,
            "все прощания ушли одним всплеском ({:?}) — потеряются вместе, это одна попытка, а не {BYE_REPEATS}",
            last.duration_since(started)
        );
    }

    /// ПОДСКАЗКА КООРДИНАТОРА УКОРАЧИВАЕТ СРОК ТИШИНЫ. Второй слой обнаружения:
    /// пира могли убить, и прощание по UDP послать было некому — зато у
    /// координатора рвётся сокет, и он об этом говорит.
    #[tokio::test(start_paused = true)]
    async fn a_hint_shortens_the_silence_deadline() {
        let (a, b) = memory_pair(64);
        let ka = std::sync::Arc::new(KeepaliveLink::new(a));
        let _dead_peer = b; // пира «убили»: держим конец провода, но молчим

        let started = tokio::time::Instant::now();
        let rx = {
            let link = ka.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                (link.recv_into(&mut buf).await.unwrap(), tokio::time::Instant::now())
            })
        };
        tokio::task::yield_now().await;
        ka.check_peer_now();

        let (alive, ended) = rx.await.unwrap();
        let took = ended.duration_since(started);
        assert!(!alive, "молчащий пир обязан кончиться EOF");
        assert!(took < DEAD_AFTER, "подсказка не укоротила срок тишины: ждали {took:?} ≈ обычные {DEAD_AFTER:?}");
        assert!(
            took >= PEER_CHECK_GRACE,
            "срок ужат СИЛЬНЕЕ, чем живой пир успевает доказать жизнь ({took:?} < {PEER_CHECK_GRACE:?}) — так подсказка начнёт рвать живые сессии",
        );
    }

    /// ПОДСКАЗКА — НЕ ПРИКАЗ. Живая сессия обязана пережить её без единой
    /// царапины: иначе координатор (и всякий, кто сумеет к нему подделаться)
    /// получил бы кнопку удалённого отключения чужого туннеля.
    #[tokio::test(start_paused = true)]
    async fn a_hint_never_kills_a_live_peer() {
        let (a, b) = memory_pair(64);
        let ka = std::sync::Arc::new(KeepaliveLink::new(a));
        let kb = std::sync::Arc::new(KeepaliveLink::new(b));
        // Живой пир: крутит приём, значит шлёт свои keepalive и отвечает на PROBE.
        let peer = kb.clone();
        tokio::spawn(async move {
            let mut buf = Vec::new();
            let _ = peer.recv_into(&mut buf).await;
        });

        let rx = {
            let link = ka.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                link.recv_into(&mut buf).await
            })
        };
        tokio::task::yield_now().await;
        // Подсказок хоть десяток — живая сессия не должна их заметить.
        for _ in 0..10 {
            ka.check_peer_now();
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        assert!(
            tokio::time::timeout(Duration::from_secs(20), rx).await.is_err(),
            "живую сессию оборвало по подсказке координатора — это уже кнопка удалённого отключения, а не подсказка",
        );
    }
}
