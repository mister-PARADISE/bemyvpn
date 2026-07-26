//! bmv-common — общие примитивы для всех слоёв BeMyVPN.
//!
//! Здесь живут вещи, от которых зависят несколько крейтов, но которые сами ни от
//! кого не зависят: типы ошибок, «провод» между пирами (`Link`) и генерация id.
//! Держим этот крейт тонким — это фундамент, а не свалка.

pub mod error;
pub mod ids;
pub mod wire;

pub use error::{Error, Result};
pub use wire::{KeepaliveLink, Link};
