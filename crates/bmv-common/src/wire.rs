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
}

// ── Keepalive: детект обрыва поверх любого Link ──────────────────────────────

/// Маркер живости — один нулевой байт. Настоящий IP-пакет так не выглядит
/// (минимум 20 байт, версия 4/6 в старшем ниббле), поэтому не спутать с данными.
const KEEPALIVE: [u8; 1] = [0u8];
/// Маркер прощания: сторона закрывает канал СОЗНАТЕЛЬНО. Другая сторона видит
/// EOF мгновенно, не дожидаясь keepalive-таймаута (важно для живого счётчика
/// гостей: отключился — каталог узнаёт сразу).
const BYE: [u8; 1] = [1u8];
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

/// Обёртка `Link`, дающая детект обрыва (UDP сам разрыв не сигналит).
///
/// Фоном шлёт пирy keepalive-пустышки; в `recv` фильтрует их и обновляет
/// «время последней активности». Если от пира тишина дольше `DEAD_AFTER` —
/// `recv` возвращает пустой `Vec` (EOF), и туннель штатно завершает сессию.
pub struct KeepaliveLink {
    inner: std::sync::Arc<dyn Link>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
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
        KeepaliveLink { inner, stop }
    }
}

impl Drop for KeepaliveLink {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

#[async_trait]
impl Link for KeepaliveLink {
    async fn send(&self, packet: &[u8]) -> Result<()> {
        self.inner.send(packet).await
    }

    async fn recv_into(&self, buf: &mut Vec<u8>) -> Result<bool> {
        loop {
            match tokio::time::timeout(DEAD_AFTER, self.inner.recv_into(buf)).await {
                Ok(Ok(false)) => return Ok(false),                        // канал закрыт
                Ok(Ok(true)) if buf.as_slice() == KEEPALIVE => continue,  // пустышка — глотаем
                // Пир СОЗНАТЕЛЬНО попрощался (чистый выход) — EOF мгновенно.
                Ok(Ok(true)) if buf.as_slice() == BYE => return Ok(false),
                Ok(Ok(true)) => return Ok(true),
                Ok(Err(e)) => return Err(e),
                // Тишина дольше DEAD_AFTER → пир мёртв (резкий обрыв без BYE).
                Err(_) => return Ok(false),
            }
        }
    }

    async fn close(&self) -> Result<()> {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        // Прощаемся дважды (UDP может потерять одиночный пакет) — пир увидит EOF
        // мгновенно вместо ожидания keepalive-таймаута.
        let _ = self.inner.send(&BYE).await;
        let _ = self.inner.send(&BYE).await;
        self.inner.close().await
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
            .map_err(|_| crate::Error::other("memory link: другой конец закрыт"))
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
}
