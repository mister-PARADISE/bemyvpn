//! bmv-protocol — ОСЬ ГИБКОСТИ. Один трейт `Protocol`, много плагинов.
//!
//! Протокол берёт сырой (нешифрованный) канал к пиру и возвращает новый —
//! (возможно) шифрованный/замаскированный. Он и есть «шифрование + маскировка».
//! Выбор — за конфигом. Добавить свой = один файл + строка в `Registry`.
//!
//! Встроенные:
//!   noise  — рабочее шифрование (Noise XX, X25519+ChaCha20Poly1305)
//!   plain  — без шифрования (скорость/доверие/тест)
//!   (позже) reality — TLS-мимикрия через внешний sing-box; webrtc — под звонок.

use std::sync::Arc;

use async_trait::async_trait;
use bmv_common::{Link, Result};

mod noise;
/// Замороженная предыдущая версия `noise` — эталон провода для тестов
/// совместимости (см. `noise_v0_frozen` и `wire_compat` в `noise.rs`).
#[cfg(test)]
mod noise_v0_frozen;
mod plain;
pub use noise::Noise;
pub use plain::Plain;

/// Контракт протокола — «стандарт», которому следуют все плагины.
/// Оборачивает сырой `Link` в (возможно) шифрованный. Обе стороны знакомы —
/// сам handshake (если есть) идёт прямо по каналу.
#[async_trait]
pub trait Protocol: Send + Sync {
    /// Короткое имя (совпадает с тем, что пишут в конфиге).
    fn name(&self) -> &'static str;

    /// Шифрует ли трафик — для UI (показать замочек) и предупреждений.
    fn encrypts(&self) -> bool;

    /// Доступен ли в этом окружении (есть зависимости, напр. sing-box).
    fn available(&self) -> bool {
        true
    }

    /// ХОСТ (отвечающая сторона): обернуть канал.
    async fn connect_host(&self, link: Box<dyn Link>) -> Result<Box<dyn Link>>;

    /// ГОСТЬ (инициатор): обернуть канал.
    async fn connect_guest(&self, link: Box<dyn Link>) -> Result<Box<dyn Link>>;
}

/// Реестр протоколов. Порядок = порядок внутреннего фолбэка
/// (`reality → webrtc → wireguard → plain`), самый стойкий первым.
pub struct Registry {
    protocols: Vec<Arc<dyn Protocol>>,
}

impl Registry {
    /// Собрать реестр из встроенных протоколов.
    pub fn with_builtins() -> Self {
        let protocols: Vec<Arc<dyn Protocol>> = vec![
            // Порядок = приоритет фолбэка. noise — ChaCha20 (дефолт), noise-obfs —
            // «Маскировка» (сильнее против DPI), plain — без шифра. AES убран как
            // избыточный (ChaCha20 быстр и работает везде).
            Arc::new(Noise::chacha()),
            Arc::new(Noise::obfs()),
            Arc::new(Plain),
        ];
        Registry { protocols }
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Protocol>> {
        self.protocols.iter().find(|p| p.name() == name).cloned()
    }

    /// Имена всех известных протоколов (для каталога/UI).
    pub fn names(&self) -> Vec<&'static str> {
        self.protocols.iter().map(|p| p.name()).collect()
    }

    /// Имена доступных в этом окружении протоколов.
    pub fn available(&self) -> Vec<&'static str> {
        self.protocols
            .iter()
            .filter(|p| p.available())
            .map(|p| p.name())
            .collect()
    }

    /// Порядок фолбэка начиная с `preferred`: сперва выбранный (если доступен),
    /// затем остальные доступные в порядке реестра.
    pub fn fallback_order(&self, preferred: &str) -> Vec<Arc<dyn Protocol>> {
        let mut order = Vec::new();
        if let Some(p) = self.get(preferred) {
            if p.available() {
                order.push(p);
            }
        }
        for p in &self.protocols {
            if p.available() && p.name() != preferred {
                order.push(p.clone());
            }
        }
        order
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::with_builtins()
    }
}
