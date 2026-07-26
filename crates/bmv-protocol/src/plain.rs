//! plain — без шифрования. Сырой путь отдаётся как есть.
//!
//! Зачем: максимальная скорость в доверенной сети/LAN, эталон производительности,
//! отладка. Осознанный выбор «без шифра» — не баг, а фича (см. конфиг).

use async_trait::async_trait;
use bmv_common::{Link, Result};

use crate::Protocol;

pub struct Plain;

#[async_trait]
impl Protocol for Plain {
    fn name(&self) -> &'static str {
        "plain"
    }

    fn encrypts(&self) -> bool {
        false
    }

    async fn connect_host(&self, link: Box<dyn Link>) -> Result<Box<dyn Link>> {
        Ok(link)
    }

    async fn connect_guest(&self, link: Box<dyn Link>) -> Result<Box<dyn Link>> {
        Ok(link)
    }
}
