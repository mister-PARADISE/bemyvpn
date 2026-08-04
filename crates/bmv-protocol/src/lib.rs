//! bmv-protocol — шифрование и маскировка. Протокол берёт сырой канал к пиру и
//! возвращает новый — (возможно) шифрованный/замаскированный.
//!
//! Их РОВНО ТРИ, все здесь:
//!   `noise`      — Noise XX, X25519 + ChaCha20-Poly1305 (по умолчанию);
//!   `noise-obfs` — то же шифрование плюс маскировка от DPI («Маскировка»);
//!   `plain`      — без шифрования (скорость в доверенной сети, эталон, отладка).
//!
//! Шапка обещала здесь «много плагинов», а док `Registry` — фолбэк
//! `reality → webrtc → wireguard → plain`. Ничего из этих трёх в коде нет и не
//! было, как не было и самого фолбэка: перебирать протоколы не с кем — обе
//! стороны берут РОВНО ОДНО имя из настройки, а договориться на лету по UDP
//! нельзя. Цена обещания не нулевая: имя протокола приходит из конфига строкой,
//! и написавший в конфиге `reality` получал `noise` без единого слова. Поэтому
//! список тут ровно тот, что есть, а порядок реестра — это порядок ПОКАЗА
//! (`display_order`), и так он теперь и называется.

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

/// Реестр протоколов: `noise → noise-obfs → plain` (закреплён тестом
/// `registry_order_is_the_order_on_screen`).
///
/// Порядок — ЭТО ПОРЯДОК ПОКАЗА, и только он. Никакого перебора при неудаче
/// нет: обе стороны берут ровно одно имя из настройки, договориться на лету по
/// UDP не с кем (см. `BmvEngine::connect_order` в `bmv-core`).
pub struct Registry {
    protocols: Vec<Arc<dyn Protocol>>,
}

impl Registry {
    /// Собрать реестр из встроенных протоколов.
    pub fn with_builtins() -> Self {
        let protocols: Vec<Arc<dyn Protocol>> = vec![
            // Порядок = порядок в списке на экране. noise — ChaCha20 (дефолт),
            // noise-obfs — «Маскировка» (сильнее против DPI), plain — без шифра.
            // AES убран как избыточный (ChaCha20 быстр и работает везде).
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

    /// Порядок ПОКАЗА начиная с `preferred`: сперва выбранный (если доступен),
    /// затем остальные доступные в порядке реестра.
    ///
    /// Звалось `fallback_order` — и это было единственное место во всём ядре,
    /// где слово «фолбэк» ещё что-то утверждало. Перебора нет: результат
    /// уходит ровно в один вызов — терминальную команду `protocols`, которая
    /// печатает список. Подключение берёт имя напрямую (`protocol_by_name`).
    pub fn display_order(&self, preferred: &str) -> Vec<Arc<dyn Protocol>> {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// ПОРЯДОК РЕЕСТРА — ЭТО ПОРЯДОК СПИСКА НА ЭКРАНЕ, и больше ничего.
    ///
    /// Прежнее имя этого теста — `registry_order_is_the_fallback_order` — было
    /// последним местом, где перебор протоколов ещё считался существующим.
    /// Его нет и не было: обе стороны берут РОВНО ОДНО имя из настройки
    /// (`protocol_by_name`), договориться на лету по UDP не с кем. Зелёный тест
    /// читают доверчивее комментария, поэтому ложь в ИМЕНИ живёт дольше всего.
    #[test]
    fn registry_order_is_the_order_on_screen() {
        let r = Registry::with_builtins();
        assert_eq!(r.names(), ["noise", "noise-obfs", "plain"]);

        let order: Vec<_> = r.display_order("noise-obfs").iter().map(|p| p.name()).collect();
        assert_eq!(order, ["noise-obfs", "noise", "plain"], "выбранный первым, остальные — в порядке реестра");

        // Имя из конфига могли набрать руками. Незнакомое не должно ломать
        // СПИСОК — но и подключением оно не станет: подключение сюда не ходит.
        let order: Vec<_> = r.display_order("такого-нет").iter().map(|p| p.name()).collect();
        assert_eq!(order, ["noise", "noise-obfs", "plain"]);
    }

    /// А вот и то, чем перебор НЕ является: незнакомое имя не превращается в
    /// первый протокол списка. Если когда-нибудь `get` начнёт «подбирать
    /// похожее», этот тест покраснеет раньше, чем кто-то уедет в `plain`,
    /// думая, что включил `reality`.
    #[test]
    fn an_unknown_name_resolves_to_nothing_at_all() {
        let r = Registry::with_builtins();
        assert!(r.get("reality").is_none(), "несуществующий протокол не смеет находиться");
        assert!(r.get("wireguard").is_none());
        assert!(r.get("webrtc").is_none());
    }
}
