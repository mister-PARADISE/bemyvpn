//! bmv-net — ЗНАКОМСТВО и NAT. Узнать свой внешний адрес и пробить прямой путь.
//!
//!   • STUN-пул — свой внешний IP:port (публичные серверы + координатор);
//!   • hole-punching — прямой UDP-путь к пиру по кандидатам;
//!   • отдаёт наверх сырой `Link` (UDP), который протокол обернёт шифрованием.
//!
//! Relay намеренно нет: только прямое P2P (трафик мимо любого сервера).

pub mod stun;
pub mod udp;

pub use stun::{classify_mapping, reflexive_addr, reflexive_addr_on, DEFAULT_STUN};
pub use udp::{local_ip, punch_tokens, UdpEndpoint, UdpHub, UdpLink};
