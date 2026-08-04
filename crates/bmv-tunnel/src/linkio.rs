//! LinkIo — мост между датаграммным `Link` и `AsyncRead + AsyncWrite`.
//!
//! `ipstack` хочет «устройство» как байтовый поток, где каждый read/write —
//! один IP-пакет (как у TUN). Наш `Link` датаграммный (send/recv пакет целиком),
//! так что адаптер один-в-один: read = один `Link::recv`, write = один
//! `Link::send`. Границы пакетов сохраняются.
//!
//! Две тонкости записи:
//!
//! 1. Отправку в сеть делает ОТДЕЛЬНАЯ фоновая задача (её кормит очередь). Петля
//!    `ipstack` — одна задача с `select!` между чтением устройства (ACK гостя) и
//!    записью (данные гостю); если бы запись блокировалась на переполненном
//!    UDP-буфере, петля перестала бы читать ACK'и и поток встал бы. Поэтому в сеть
//!    пишет фоновый сток, а `poll_write` лишь кладёт пакет в очередь.
//!
//! 2. ПЕЙСИНГ (опциональный). Некоторые провайдеры полисят исходящий UDP по
//!    пакетам-в-секунду токен-бакетом (замеряли ~2800 pps): burst проходит,
//!    дальше режется → потеря → слабый ретрансмит ipstack не вывозит → поток
//!    встаёт. Для ТАКИХ хостов есть пейсинг чуть ниже лимита. НО на нормальных
//!    сетях он зря режет скорость, поэтому ПО УМОЛЧАНИЮ ВЫКЛЮЧЕН — темп задаёт
//!    само TCP-окно гостя. Включается env `BMV_TX_PPS=<пакетов/с>` (0 = без
//!    пейсинга). При нехватке токенов `poll_write` возвращает Pending (тормозит
//!    ipstack, не теряет пакет).

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bmv_common::Link;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant, Sleep};
use tokio_util::sync::PollSender;

/// Разрешённый мгновенный всплеск (пакетов) при включённом пейсинге.
const BURST: f64 = 50.0;
/// Глубина очереди к фоновому стоку.
const SEND_QUEUE: usize = 4096;
/// Глубина приёмной очереди (readahead: UDP-приём не ждёт, пока ipstack
/// переварит предыдущий пакет).
const RECV_QUEUE: usize = 512;
/// Потолок пула переиспользуемых буферов пакета.
const POOL_CAP: usize = RECV_QUEUE + 8;

/// Пул буферов под один пакет. ОБА направления берут отсюда и возвращают сюда,
/// поэтому в устоявшемся режиме перекачка не аллоцирует вовсе.
///
/// Раньше пул был только на приёме, а `poll_write` делал `buf.to_vec()` — то
/// есть КАЖДЫЙ пакет, уходящий гостю (а это направление скачивания, самое
/// нагруженное), стоил одной аллокации и одного освобождения. Объяснить эту
/// разницу между направлениями было нечем: буфер там и там один и тот же, MTU.
#[derive(Default)]
struct BufPool(std::sync::Mutex<Vec<Vec<u8>>>);

impl BufPool {
    /// Буфер под `recv_into`: из пула, а если пул пуст (или отравлен паникой) —
    /// свежий. ДЛИНУ НЕ СБРАСЫВАЕМ.
    ///
    /// `clear()` тут выглядел безобидно, но стоил полного MTU обнуления на
    /// КАЖДЫЙ принятый пакет: `NoiseLink::recv_into` растёт по потребности
    /// (`if out.len() < frame.len() { out.resize(frame.len(), 0) }`, см.
    /// noise.rs), и с прежней длиной он зануляет только дельту — 24 байта
    /// (nonce+tag), а с нулевой — все 1424. Содержимое наружу не течёт: по
    /// контракту `Link::recv_into` (bmv-common/src/wire.rs) реализация
    /// ПЕРЕЗАПИСЫВАЕТ буфер и сама выставляет длину.
    fn empty(&self) -> Vec<u8> {
        self.0.lock().ok().and_then(|mut p| p.pop()).unwrap_or_default()
    }

    /// Буфер с копией `data`. Вот ЗДЕСЬ сброс обязателен — дописываем к хвосту.
    fn filled(&self, data: &[u8]) -> Vec<u8> {
        let mut b = self.empty();
        b.clear();
        b.extend_from_slice(data);
        b
    }

    /// Вернуть отработавший буфер. Сверх потолка отдаём аллокатору — пул не
    /// должен расти на всплеске и держать память до конца сессии.
    fn give(&self, b: Vec<u8>) {
        if let Ok(mut p) = self.0.lock() {
            if p.len() < POOL_CAP {
                p.push(b);
            }
        }
    }
}

/// Целевой темп отправки из env `BMV_TX_PPS` (пакетов/с). 0/пусто → пейсинг ВЫКЛ.
fn tx_pps() -> f64 {
    std::env::var("BMV_TX_PPS")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|v| *v > 0.0)
        .unwrap_or(0.0)
}

pub struct LinkIo {
    /// Приёмная очередь: постоянная фоновая задача зовёт `link.recv()` и кормит
    /// канал; `poll_read` лишь опрашивает его (без пересоздания boxed-future и
    /// аллокаций на каждый пакет, как было раньше). Закрытие канала = EOF.
    recv_rx: mpsc::Receiver<Vec<u8>>,
    /// Очередь к фоновому стоку. PollSender даёт НАСТОЯЩИЙ backpressure: когда
    /// очередь полна, `poll_write` возвращает Pending (а не «отправил» с тихим
    /// дропом) — ipstack притормаживает чтение, его TCP-окно закрывается само.
    send_tx: PollSender<Vec<u8>>,
    // Токен-бакет пейсинга (живёт в poll_write). rate<=0 → пейсинг выключен.
    rate: f64,
    tokens: f64,
    last: Instant,
    sleep: Option<Pin<Box<Sleep>>>,
    /// Одноразовый сигнал «канал умер» — по нему хост прерывает свой стек.
    /// (ipstack сам EOF устройства не обрабатывает, поэтому сносим его снаружи.)
    dead: Option<tokio::sync::oneshot::Sender<()>>,
    eof: bool,
    /// Буферы пакетов обоих направлений (см. `BufPool`).
    pool: Arc<BufPool>,
    /// Гард отмены фоновых задач (см. `AbortBoth`).
    _abort: AbortBoth,
}

/// Абортит фоновые сток и приёмник при уничтожении `LinkIo`.
///
/// Без него задачи переживают сессию, ДЕРЖА `Arc<dyn Link>`: приёмник висит в
/// `recv_into` и замечает смерть `LinkIo` только на СЛЕДУЮЩЕМ пришедшем пакете —
/// то есть до keepalive-интервала (а если пир уже молчит, то до `DEAD_AFTER`,
/// восемь секунд). Всё это время канал не дропается, а значит и прощание (BYE) из
/// его `Drop` не уходит: хост, погасивший раздачу, прощался с гостями с задержкой
/// в целый таймаут — ровно тогда, когда прощание уже никому не нужно.
struct AbortBoth(tokio::task::AbortHandle, tokio::task::AbortHandle);
impl Drop for AbortBoth {
    fn drop(&mut self) {
        self.0.abort();
        self.1.abort();
    }
}

impl LinkIo {
    pub fn new(link: Arc<dyn Link>, dead: tokio::sync::oneshot::Sender<()>) -> Self {
        let pool: Arc<BufPool> = Arc::new(BufPool::default());
        let (send_tx, mut send_rx) = mpsc::channel::<Vec<u8>>(SEND_QUEUE);
        // Единственный, кто реально зовёт link.send. Пейсинг — на входе в очередь
        // (в poll_write), поэтому здесь просто проталкиваем в сеть по мере сил.
        let sender = link.clone();
        let spool = pool.clone();
        let send_task = tokio::spawn(async move {
            while let Some(pkt) = send_rx.recv().await {
                let ok = sender.send(&pkt).await.is_ok();
                spool.give(pkt); // буфер ушёл в сеть — назад в пул
                if !ok {
                    break; // канал мёртв; смерть заметит и recv-путь (keepalive)
                }
            }
        });
        // Единственный, кто зовёт link.recv: кормит приёмную очередь. Обрыв
        // (EOF/ошибка/keepalive-смерть) → задача выходит, tx дропается, канал
        // закрывается → poll_read увидит None и объявит сессию мёртвой.
        let (recv_tx, recv_rx) = mpsc::channel::<Vec<u8>>(RECV_QUEUE);
        let receiver = link;
        let rpool = pool.clone();
        let recv_task = tokio::spawn(async move {
            loop {
                // Буфер из пула (или свежий, если пусто) — заполняем recv_into.
                let mut buf = rpool.empty();
                match receiver.recv_into(&mut buf).await {
                    Ok(true) => {
                        if recv_tx.send(buf).await.is_err() {
                            break; // LinkIo дропнут — сессии больше нет
                        }
                    }
                    _ => break, // Ok(false)=закрыт или ошибка/keepalive-смерть
                }
            }
        });
        LinkIo {
            recv_rx,
            send_tx: PollSender::new(send_tx),
            rate: tx_pps(),
            tokens: BURST,
            last: Instant::now(),
            sleep: None,
            dead: Some(dead),
            eof: false,
            pool,
            _abort: AbortBoth(send_task.abort_handle(), recv_task.abort_handle()),
        }
    }

    fn mark_dead(&mut self) {
        self.eof = true;
        if let Some(tx) = self.dead.take() {
            let _ = tx.send(());
        }
    }
}

impl AsyncRead for LinkIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.eof {
            return Poll::Pending; // мёртв — паркуемся, стек прервут снаружи по сигналу
        }
        match self.recv_rx.poll_recv(cx) {
            Poll::Ready(Some(pkt)) => {
                if pkt.len() > buf.remaining() {
                    // Не должно случаться (ipstack всегда даёт буфер ≥ MTU). Но если
                    // случится — обрезать IP-пакет нельзя (порча, не пройдёт checksum
                    // → тихо теряется). Роняем пакет ЦЕЛИКОМ (как сетевую потерю,
                    // TCP ретрансмитит) и будим себя за следующим — но НЕ EOF.
                    tracing::warn!("linkio: пакет {} Б > буфера {} Б — дропнут целиком", pkt.len(), buf.remaining());
                    cx.waker().wake_by_ref();
                    return Poll::Pending;
                }
                buf.put_slice(&pkt);
                self.pool.give(pkt); // буфер отработал — назад в пул
                Poll::Ready(Ok(()))
            }
            Poll::Ready(None) => {
                self.mark_dead(); // приёмная задача вышла: EOF/ошибка/keepalive
                Poll::Pending
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl AsyncWrite for LinkIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        // Пейсинг только если задан темп (env BMV_TX_PPS). Иначе — полная
        // скорость: темп задаёт TCP-окно гостя, ipstack не всплескивает сверх него.
        if self.rate > 0.0 {
            let rate = self.rate;
            // Пополняем токены по прошедшему времени.
            let now = Instant::now();
            self.tokens = (self.tokens + now.duration_since(self.last).as_secs_f64() * rate).min(BURST);
            self.last = now;

            // Нет токена — тормозим ipstack (Pending), ждём таймер, НЕ теряем пакет.
            if self.tokens < 1.0 {
                if self.sleep.is_none() {
                    let wait = Duration::from_secs_f64((1.0 - self.tokens) / rate);
                    self.sleep = Some(Box::pin(tokio::time::sleep(wait)));
                }
                let ready = self.sleep.as_mut().unwrap().as_mut().poll(cx).is_ready();
                if !ready {
                    return Poll::Pending;
                }
                self.sleep = None;
                self.tokens = 1.0; // проспали ровно один токен
            }
            self.tokens -= 1.0;
        }

        // Backpressure вместо тихого дропа: резервируем слот в очереди. Полна →
        // Pending (ipstack перестанет читать upstream, его окно закроется — это и
        // есть правильный flow control). Пакет НЕ теряем и не выдаём фальшивый Ok
        // (иначе ipstack взводил RTO на сегмент, который не ушёл, → столл).
        match self.send_tx.poll_reserve(cx) {
            Poll::Ready(Ok(())) => {}
            Poll::Ready(Err(_)) => {
                return Poll::Ready(Err(io::Error::new(io::ErrorKind::BrokenPipe, "Связь с хостом оборвалась.")));
            }
            Poll::Pending => return Poll::Pending,
        }
        // Буфер из общего пула, а не свежий `Vec` на каждый пакет: слот в очереди
        // уже зарезервирован, значит буфер точно уедет и вернётся (см. `BufPool`).
        let pkt = self.pool.filled(buf);
        match self.send_tx.send_item(pkt) {
            Ok(()) => Poll::Ready(Ok(buf.len())),
            Err(_) => Poll::Ready(Err(io::Error::new(io::ErrorKind::BrokenPipe, "Связь с хостом оборвалась."))),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    /// ОТПРАВКА ГОСТЮ НЕ АЛЛОЦИРУЕТ НА КАЖДЫЙ ПАКЕТ.
    ///
    /// Это направление скачивания — самое нагруженное в туннеле: при 100 Мбит/с
    /// через хост проходит порядка десяти тысяч пакетов в секунду, и `to_vec()`
    /// на каждом означал столько же пар «выделил-освободил» на ровном месте, при
    /// том что на приёме буферы переиспользовались уже давно.
    ///
    /// Проверяем по КРУГОВОРОТУ: один и тот же буфер обязан уходить в сеть,
    /// возвращаться в пул и уходить снова. Считать аллокации напрямую нечем, зато
    /// это видно по пулу: при переиспользовании там всё время лежит ОДИН и тот же
    /// буфер (его забрали и вернули), а стоит отправке начать делать свой `Vec` на
    /// каждый пакет — пул начнёт РАСТИ чужими буферами, и счёт разойдётся сразу.
    #[tokio::test]
    async fn the_send_path_reuses_buffers_instead_of_allocating() {
        let (mine, _peer) = bmv_common::wire::memory_pair(256);
        let (dead_tx, _dead_rx) = tokio::sync::oneshot::channel();
        let mut io = LinkIo::new(Arc::from(mine), dead_tx);
        let pkt = vec![7u8; 1400]; // пакет в целый MTU, как в жизни

        // Разгон: первый буфер забирает себе приёмник — он держит один наготове,
        // пока ждёт пакет от пира (это его работа, а не утечка). Со второго круга
        // в пуле остаётся ровно тот буфер, что вернула отправка.
        for _ in 0..2 {
            io.write_all(&pkt).await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await; // сток разбирает очередь
        }
        let first = {
            let free = io.pool.0.lock().unwrap();
            assert_eq!(free.len(), 1, "буфер не вернулся в пул после отправки");
            assert!(free[0].capacity() >= pkt.len(), "в пул вернулся не тот буфер, что уходил в сеть");
            free[0].as_ptr()
        };

        for n in 3..=8 {
            io.write_all(&pkt).await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
            let free = io.pool.0.lock().unwrap();
            assert_eq!(
                free.len(), 1,
                "после {n}-й отправки в пуле {} буферов — отправка не берёт буфер оттуда, \
                 а выделяет новый на КАЖДЫЙ пакет (это направление скачивания, ~10 000 пакетов/с)",
                free.len()
            );
            assert_eq!(free[0].as_ptr(), first, "в сеть ушёл не тот буфер, что лежал в пуле");
        }
    }

    /// ПРИЁМ НЕ ОБНУЛЯЕТ БУФЕР ЗАНОВО НА КАЖДЫЙ ПАКЕТ.
    ///
    /// `recv_into` у `NoiseLink` растёт по потребности — `if out.len() <
    /// frame.len() { out.resize(frame.len(), 0) }`. Кадр длиннее plaintext ровно
    /// на 8 байт nonce и 16 байт tag, поэтому с буфером ПРЕЖНЕЙ длины обнуляются
    /// 24 байта, а с обнулённым — все 1424. Разница в 59 раз на пакет, и на
    /// десяти тысячах пакетов в секунду это уже мегабайты memset на ровном месте.
    ///
    /// Ловим за длину: `empty()` обязан отдавать буфер как есть, `filled()` —
    /// сбрасывать (он дописывает к хвосту, и без сброса склеил бы два пакета).
    #[test]
    fn the_recv_path_does_not_re_zero_the_whole_mtu() {
        let pool = BufPool::default();
        pool.give(vec![9u8; 1400]); // «отработавший» буфер прошлого пакета

        let b = pool.empty();
        assert_eq!(
            b.len(), 1400,
            "буфер приёма пришёл с длиной {} вместо 1400: значит его сбросили, и шифрослой \
             занулит целый MTU на КАЖДЫЙ пакет вместо 24 байт (см. resize в noise.rs)",
            b.len()
        );

        // А отправка обязана получить ровно свои данные, без хвоста прошлого пакета.
        pool.give(b);
        assert_eq!(pool.filled(&[1, 2, 3]), [1, 2, 3], "к отправке приклеился хвост прошлого пакета");
    }
}
