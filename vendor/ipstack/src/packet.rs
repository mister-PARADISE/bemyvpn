use crate::error::IpStackError;
use etherparse::{Ipv4Header, Ipv6Header, NetSlice, SlicedPacket, TcpHeader, UdpHeader};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

#[derive(Eq, Hash, PartialEq, Debug, Clone, Copy)]
pub struct NetworkTuple {
    pub src: SocketAddr,
    pub dst: SocketAddr,
    pub tcp: bool,
}

impl NetworkTuple {
    pub fn new(src: SocketAddr, dst: SocketAddr, tcp: bool) -> Self {
        NetworkTuple { src, dst, tcp }
    }
}

impl std::fmt::Display for NetworkTuple {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tcp = if self.tcp { "TCP" } else { "UDP" };
        write!(f, "{} {} -> {}", tcp, self.src, self.dst)
    }
}

pub mod tcp_flags {
    pub const CWR: u8 = 0b10000000;
    pub const ECE: u8 = 0b01000000;
    pub const URG: u8 = 0b00100000;
    pub const ACK: u8 = 0b00010000;
    pub const PSH: u8 = 0b00001000;
    pub const RST: u8 = 0b00000100;
    pub const SYN: u8 = 0b00000010;
    pub const FIN: u8 = 0b00000001;
}

#[derive(Debug, Clone)]
pub(crate) enum IpHeader {
    Ipv4(Ipv4Header),
    Ipv6(Ipv6Header),
}

#[derive(Debug, Clone)]
pub(crate) enum TransportHeader {
    Tcp(TcpHeader),
    Udp(UdpHeader),
    Unknown,
}

#[derive(Debug, Clone)]
pub struct NetworkPacket {
    pub(crate) ip: IpHeader,
    pub(crate) transport: TransportHeader,
    pub(crate) payload: Option<Vec<u8>>,
}

impl NetworkPacket {
    pub fn parse(buf: &[u8]) -> Result<Self, IpStackError> {
        let p = SlicedPacket::from_ip(buf).map_err(|_| IpStackError::InvalidPacket)?;
        let ip = p.net.ok_or(IpStackError::InvalidPacket)?;

        let (ip, ip_payload) = match ip {
            NetSlice::Ipv4(ip) => (IpHeader::Ipv4(ip.header().to_header()), ip.payload().payload),
            NetSlice::Ipv6(ip) => (IpHeader::Ipv6(ip.header().to_header()), ip.payload().payload),
            NetSlice::Arp(_) => return Err(IpStackError::UnsupportedTransportProtocol),
        };
        let (transport, payload) = match p.transport {
            Some(etherparse::TransportSlice::Tcp(h)) => (TransportHeader::Tcp(h.to_header()), h.payload()),
            Some(etherparse::TransportSlice::Udp(u)) => (TransportHeader::Udp(u.to_header()), u.payload()),
            _ => (TransportHeader::Unknown, ip_payload),
        };
        let payload = if payload.is_empty() { None } else { Some(payload.to_vec()) };

        Ok(NetworkPacket { ip, transport, payload })
    }
    pub(crate) fn transport_header(&self) -> &TransportHeader {
        &self.transport
    }
    pub fn src_addr(&self) -> SocketAddr {
        let port = match &self.transport {
            TransportHeader::Udp(udp) => udp.source_port,
            TransportHeader::Tcp(tcp) => tcp.source_port,
            _ => 0,
        };
        match &self.ip {
            IpHeader::Ipv4(ip) => SocketAddr::new(IpAddr::V4(Ipv4Addr::from(ip.source)), port),
            IpHeader::Ipv6(ip) => SocketAddr::new(IpAddr::V6(Ipv6Addr::from(ip.source)), port),
        }
    }
    pub fn dst_addr(&self) -> SocketAddr {
        let port = match &self.transport {
            TransportHeader::Udp(udp) => udp.destination_port,
            TransportHeader::Tcp(tcp) => tcp.destination_port,
            _ => 0,
        };
        match &self.ip {
            IpHeader::Ipv4(ip) => SocketAddr::new(IpAddr::V4(Ipv4Addr::from(ip.destination)), port),
            IpHeader::Ipv6(ip) => SocketAddr::new(IpAddr::V6(Ipv6Addr::from(ip.destination)), port),
        }
    }
    pub fn network_tuple(&self) -> NetworkTuple {
        NetworkTuple {
            src: self.src_addr(),
            dst: self.dst_addr(),
            tcp: matches!(self.transport, TransportHeader::Tcp(_)),
        }
    }
    pub fn reverse_network_tuple(&self) -> NetworkTuple {
        NetworkTuple {
            src: self.dst_addr(),
            dst: self.src_addr(),
            tcp: matches!(self.transport, TransportHeader::Tcp(_)),
        }
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, IpStackError> {
        let mut buf = Vec::new();
        match self.ip {
            IpHeader::Ipv4(ref ip) => ip.write(&mut buf)?,
            IpHeader::Ipv6(ref ip) => ip.write(&mut buf)?,
        }
        match self.transport {
            TransportHeader::Tcp(ref h) => h.write(&mut buf)?,
            TransportHeader::Udp(ref h) => h.write(&mut buf)?,
            _ => {}
        };

        if let Some(payload) = &self.payload {
            buf.extend_from_slice(payload);
        }
        Ok(buf)
    }
    pub fn ttl(&self) -> u8 {
        match &self.ip {
            IpHeader::Ipv4(ip) => ip.time_to_live,
            IpHeader::Ipv6(ip) => ip.hop_limit,
        }
    }
}

pub fn tcp_header_fmt(header: &TcpHeader) -> String {
    let mut flags = String::new();
    if header.cwr {
        flags.push_str("CWR ");
    }
    if header.ece {
        flags.push_str("ECE ");
    }
    if header.urg {
        flags.push_str("URG ");
    }
    if header.ack {
        flags.push_str("ACK ");
    }
    if header.psh {
        flags.push_str("PSH ");
    }
    if header.rst {
        flags.push_str("RST ");
    }
    if header.syn {
        flags.push_str("SYN ");
    }
    if header.fin {
        flags.push_str("FIN ");
    }
    format!(
        "TcpHeader {{ seq: {}, ack: {}, flags: {} }}",
        header.sequence_number,
        header.acknowledgment_number,
        flags.trim()
    )
}

pub fn tcp_header_flags(inner: &TcpHeader) -> u8 {
    let mut flags = 0;
    if inner.cwr {
        flags |= tcp_flags::CWR;
    }
    if inner.ece {
        flags |= tcp_flags::ECE;
    }
    if inner.urg {
        flags |= tcp_flags::URG;
    }
    if inner.ack {
        flags |= tcp_flags::ACK;
    }
    if inner.psh {
        flags |= tcp_flags::PSH;
    }
    if inner.rst {
        flags |= tcp_flags::RST;
    }
    if inner.syn {
        flags |= tcp_flags::SYN;
    }
    if inner.fin {
        flags |= tcp_flags::FIN;
    }

    flags
}

// pub struct UdpPacket {
//     header: UdpHeader,
// }

// impl UdpPacket {
//     pub fn inner(&self) -> &UdpHeader {
//         &self.header
//     }
// }

// impl From<&UdpHeader> for UdpPacket {
//     fn from(header: &UdpHeader) -> Self {
//         UdpPacket {
//             header: header.clone(),
//         }
//     }
// }

// BeMyVPN fork: удалён criterion-бенчмарк `mod tests` — он требовал dev-dep
// `criterion`, которого нет в Cargo.toml форка, и потому ЛОМАЛ `cargo test -p
// ipstack` целиком (единственный «тест» в модуле — обёртка запуска бенча, не
// юнит-тест).

#[cfg(test)]
mod tests {
    use super::*;
    use etherparse::{IpNumber, TcpHeader};

    /// Собрать валидный IPv4+TCP пакет. `ihl_options` — сколько байт «опций»
    /// дописать в IP-заголовок (кратно 4), чтобы проверить разбор IHL > 5.
    fn ipv4_tcp(payload: &[u8], ihl_options: usize) -> Vec<u8> {
        assert!(ihl_options % 4 == 0);
        let mut tcp = TcpHeader::new(1234, 80, 1000, 4096);
        tcp.ack = true;
        let ip_len = 20 + ihl_options;
        let mut ip = Ipv4Header::new(0, 64, IpNumber::TCP, [10, 0, 0, 1], [1, 1, 1, 1]).unwrap();
        ip.set_payload_len(tcp.header_len() + payload.len()).unwrap();

        let mut buf = Vec::new();
        ip.write(&mut buf).unwrap();
        if ihl_options > 0 {
            // Заголовок длиннее: правим IHL и total_length руками, опции — NOP (0x01).
            buf[0] = 0x40 | ((ip_len / 4) as u8);
            let total = (ip_len + tcp.header_len() + payload.len()) as u16;
            buf[2..4].copy_from_slice(&total.to_be_bytes());
            buf.splice(20..20, std::iter::repeat_n(0x01u8, ihl_options));
        }
        tcp.write(&mut buf).unwrap();
        buf.extend_from_slice(payload);
        buf
    }

    #[test]
    fn parse_rejects_truncated_input() {
        assert!(NetworkPacket::parse(&[]).is_err(), "пустой буфер");
        assert!(NetworkPacket::parse(&[0x45]).is_err(), "один байт");
        let pkt = ipv4_tcp(b"hello", 0);
        // Обрезаем по-разному: до конца IP-заголовка, до середины TCP-заголовка,
        // до середины payload — ни один вариант не должен паниковать.
        for cut in [1, 10, 19, 25, 39, pkt.len() - 1] {
            let r = NetworkPacket::parse(&pkt[..cut]);
            assert!(r.is_err(), "обрезка до {cut} байт должна быть отвергнута");
        }
    }

    #[test]
    fn parse_handles_ipv4_options() {
        // IHL = 6 (24 байта): payload обязан отсчитываться от КОНЦА опций,
        // иначе первые байты TCP-заголовка уедут в данные.
        let pkt = ipv4_tcp(b"hello", 4);
        let p = NetworkPacket::parse(&pkt).unwrap();
        assert_eq!(p.src_addr().port(), 1234);
        assert_eq!(p.dst_addr().port(), 80);
        assert_eq!(p.payload.as_deref(), Some(&b"hello"[..]));
    }

    #[test]
    fn parse_rejects_declared_length_beyond_buffer() {
        let mut pkt = ipv4_tcp(b"hello", 0);
        let total = u16::from_be_bytes([pkt[2], pkt[3]]);
        pkt[2..4].copy_from_slice(&(total + 100).to_be_bytes()); // объявили больше, чем есть
        assert!(NetworkPacket::parse(&pkt).is_err(), "объявленная длина > реальной должна отвергаться");
    }

    #[test]
    fn parse_ignores_trailing_garbage() {
        // Объявленная длина МЕНЬШЕ буфера: хвост (склейка двух пакетов в одном
        // чтении устройства) не должен попасть в TCP-поток как данные.
        let mut pkt = ipv4_tcp(b"hello", 0);
        pkt.extend_from_slice(&[0xAA; 64]);
        let p = NetworkPacket::parse(&pkt).unwrap();
        assert_eq!(p.payload.as_deref(), Some(&b"hello"[..]), "мусор за total_length утёк в данные");
    }

    #[test]
    fn parse_rejects_tcp_data_offset_past_end() {
        let mut pkt = ipv4_tcp(b"", 0);
        pkt[32] = 0xF0; // data offset = 15 (60 байт), а TCP-заголовка всего 20
        assert!(NetworkPacket::parse(&pkt).is_err(), "data offset за концом пакета");
    }

    #[test]
    fn parse_rejects_ihl_below_minimum() {
        let mut pkt = ipv4_tcp(b"hello", 0);
        pkt[0] = 0x44; // IHL = 4 → заголовок «16 байт», меньше минимума
        assert!(NetworkPacket::parse(&pkt).is_err(), "IHL < 5 должен отвергаться");
    }
}
