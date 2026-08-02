//! Раздача (хост): ВСЁ дерево задач под одним `JoinSet`.
//!
//! Раньше обе оболочки писали это дерево у себя, и обе одинаково: heartbeat,
//! встречное пробитие и accept-цикл — в задачах, а сессию КАЖДОГО гостя —
//! отдельным `tokio::spawn`. Такой spawn ни к чему не привязан, поэтому
//! «Выключить» гасило только accept-цикл: УЖЕ ПОДКЛЮЧЁННЫЕ ГОСТИ ПРОДОЛЖАЛИ
//! ходить в интернет через хозяина машины, пока сами не отвалятся. В терминале
//! было хуже: heartbeat тоже жил отдельно и каждые 10с заново чинил запись в
//! каталоге — de-announce отменялся собственным heartbeat'ом, и хост-призрак
//! висел в публичном списке до выхода из программы.
//!
//! Лечится это не новой машинерией, а тем, что сессии гостей порождаются В ТОТ
//! ЖЕ `JoinSet`: `JoinSet` при уничтожении обрывает ВСЕ свои задачи, поэтому
//! отмена (или просто `abort()`) внешней задачи гасит дерево целиком.

use std::sync::Arc;
use std::time::Duration;

use bmv_core::BmvEngine;
use tokio::task::JoinSet;

/// Как часто чинить запись в каталоге и держать NAT-дырку хаба открытой.
const HEARTBEAT: Duration = Duration::from_secs(10);

/// Крутить раздачу до обрыва хаба или до отмены внешней задачи.
///
/// Отмена: `abort()` на задаче, в которой эта функция крутится (или дроп её
/// future). Ничего дополнительно закрывать не надо — `JoinSet` внутри уносит
/// с собой heartbeat, встречное пробитие и все живые сессии гостей.
pub async fn serve_host(eng: Arc<BmvEngine>, hub: Arc<bmv_net::UdpHub>) {
    let mut set: JoinSet<()> = JoinSet::new();
    {
        let (e, h) = (eng.clone(), hub.clone());
        set.spawn(async move {
            loop {
                tokio::time::sleep(HEARTBEAT).await;
                let _ = e.host_heartbeat(&h).await;
            }
        });
    }
    {
        let (e, h) = (eng.clone(), hub.clone());
        set.spawn(async move {
            let _ = e.host_serve_punch(h).await;
        });
    }
    while let Some((peer, raw)) = hub.accept().await {
        // Подчищаем отработавшие: без этого набор рос бы на каждого гостя за всё
        // время раздачи (сутками — это тысячи мёртвых записей в памяти).
        while set.try_join_next().is_some() {}
        let e = eng.clone();
        set.spawn(async move {
            let _ = e.host_run_session(peer, raw, true).await;
        });
    }
    // Хаб закрылся — набор уничтожается здесь и уносит с собой всё дерево.
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Гость, принятый раздачей, обязан умереть ВМЕСТЕ с ней.
    ///
    /// Настоящий `serve_host` требует живого координатора и хаба, поэтому здесь
    /// проверяется ровно та конструкция, на которой он построен: задача-«гость»
    /// порождена в `JoinSet`, принадлежащий отменяемой задаче. Тест падает на
    /// прежнем виде кода — стоит заменить `set.spawn` на `tokio::spawn`, и
    /// «гость» переживёт отмену раздачи, как и было в жизни.
    #[tokio::test]
    async fn a_guest_session_dies_together_with_the_hosting_task() {
        static GUEST_ALIVE: AtomicBool = AtomicBool::new(false);
        let started = Arc::new(tokio::sync::Notify::new());
        let started2 = started.clone();

        let hosting = tokio::spawn(async move {
            let mut set: JoinSet<()> = JoinSet::new();
            set.spawn(async move {
                GUEST_ALIVE.store(true, Ordering::SeqCst);
                started2.notify_waiters();
                // «Гость качает трафик» — сам никогда не завершится.
                loop {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    GUEST_ALIVE.store(true, Ordering::SeqCst);
                }
            });
            // Внешняя задача живёт, пока жив набор.
            std::future::pending::<()>().await;
        });

        // Дожидаемся, что «гость» действительно поехал.
        tokio::time::timeout(Duration::from_secs(5), started.notified()).await.expect("гость не стартовал");
        assert!(GUEST_ALIVE.load(Ordering::SeqCst));

        hosting.abort(); // «Выключить»
        tokio::time::sleep(Duration::from_millis(50)).await;
        GUEST_ALIVE.store(false, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !GUEST_ALIVE.load(Ordering::SeqCst),
            "гость пережил остановку раздачи — значит сессии снова порождаются мимо JoinSet",
        );
    }
}
