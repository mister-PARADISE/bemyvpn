//! bmv-net — ЗНАКОМСТВО и NAT. Узнать свой внешний адрес и пробить прямой путь.
//!
//!   • STUN-пул — свой внешний IP:port (публичные серверы + координатор);
//!   • hole-punching — прямой UDP-путь к пиру по кандидатам;
//!   • отдаёт наверх сырой `Link` (UDP), который протокол обернёт шифрованием.
//!
//! Relay намеренно нет: только прямое P2P (трафик мимо любого сервера).

/// Куда ведёт чужой адрес — одна таблица диапазонов на все фильтры клиента.
pub mod reach;
pub mod stun;
pub mod udp;

pub use reach::{plausible_external, public_only, punch_target_allowed};
pub use stun::{classify_mapping, reflexive_addr, reflexive_addr_on, DEFAULT_STUN};
pub use udp::{local_ip, probe_rtt, PunchTokens, UdpEndpoint, UdpHub, UdpLink};
