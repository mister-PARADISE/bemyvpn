//! bmv-tunnel — САМ VPN. Кроссплатформенное ядро перекачки пакетов.
//!
//! Асимметрия, продуманная под «хост где угодно, без прав»:
//!
//!   ГОСТЬ (нужен TUN → права): ОС отдаёт IP-пакеты в виртуальный интерфейс;
//!     мы просто качаем их в зашифрованный `Link` и обратно. Создание самого
//!     интерфейса — забота ПЛАТФОРМЫ (desktop: crate `tun`; Android: VpnService;
//!     iOS: NEPacketTunnelProvider), ядро принимает уже готовый «источник
//!     пакетов» (`AsyncRead + AsyncWrite`) и не знает про конкретную ОС.
//!
//!   ХОСТ (БЕЗ прав, любая ОС): принимает IP-пакеты гостя и терминирует их
//!     собственным userspace TCP/IP-стеком (`ipstack`), а наружу открывает
//!     ОБЫЧНЫЕ сокеты. Ни root, ни TUN, ни iptables — работает на сервере, ПК,
//!     Android, iOS одинаково. Стек сам нарезает TCP под MTU туннеля, поэтому
//!     и MSS-костыли не нужны.
//!
//! DNS идёт через туннель автоматически: DNS-запросы гостя — это UDP-потоки,
//! которые хост открывает наружу как обычные сокеты.

use std::net::Ipv4Addr;
use std::sync::Arc;

use bmv_common::{Error, Link, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

mod host;
mod linkio;

pub use host::run_host;

/// MTU оверлея. 1400: оверхед инкапсуляции (Noise ~24 + UDP 8 + IP 20 ≈ 52)
/// укладывает внешний пакет в 1500 без фрагментации, но даёт на ~9% больше
/// полезной нагрузки на пакет, чем прежние 1280 (важно на путях с лимитом pps).
pub const MTU: u16 = 1400;

/// Адрес гостя внутри туннеля, в виде байт заголовка IPv4. Один и тот же на всех
/// платформах (`TunParams::guest`, `BmvVpnService.kt`, `PacketTunnelProvider.swift`) —
/// сходство закреплено тестом `guard_address_matches_tun_params`.
const GUEST_TUN_ADDR: [u8; 4] = [10, 7, 0, 2];

/// Пускать ли пакет, ПРИШЕДШИЙ ОТ ХОСТА, в сетевой стек гостя.
///
/// Хост — чужая машина, и в туннель он пишет что захочет. Всё, о чём его просили,
/// адресовано нашему же адресу в туннеле; пакет с любым ДРУГИМ получателем — это
/// попытка достать то, до чего снаружи не дотянуться: службы на самом гостe
/// (127.0.0.1, админки, докер) и его домашнюю сеть (роутер, NAS, камеры). Ядро
/// приняло бы такой пакет как обычный локальный, а межсетевой экран увидел бы
/// его пришедшим «изнутри», — то есть хост получал бы доступ в квартиру гостя.
///
/// Это зеркало SSRF-фильтра, который защищает хоста от гостя (см.
/// `host::dst_allowed`): доверия нет ни в одну сторону, обе сидят за фильтром.
///
/// IPv6 отбрасываем целиком: адрес у туннеля только IPv4, значит законных
/// v6-ответов не бывает в принципе.
fn from_host_allowed(pkt: &[u8]) -> bool {
    pkt.len() >= 20 && pkt[0] >> 4 == 4 && pkt[16..20] == GUEST_TUN_ADDR
}

/// Параметры виртуального интерфейса гостя (для платформы-шелла, создающего TUN).
#[derive(Clone, Debug)]
pub struct TunParams {
    pub name: String,
    pub address: Ipv4Addr,
    pub netmask: Ipv4Addr,
    pub mtu: u16,
}

impl TunParams {
    /// Дефолт для гостя: 10.7.0.2/24 на интерфейсе bmv0.
    pub fn guest() -> Self {
        TunParams {
            name: "bmv0".into(),
            address: Ipv4Addr::new(10, 7, 0, 2),
            netmask: Ipv4Addr::new(255, 255, 255, 0),
            mtu: MTU,
        }
    }
}

/// ГОСТЬ: качать IP-пакеты между интерфейсом ОС (`device`) и каналом до хоста.
///
/// `device` — уже созданный платформой источник пакетов (AsyncRead+AsyncWrite,
/// каждый read/write = один IP-пакет). Блокирует до обрыва любой из сторон.
///
/// Перекачки — ДВЕ ПАРАЛЛЕЛЬНЫЕ задачи (аплоад и даунлоад на разных ядрах,
/// без head-of-line между направлениями). Гарантия отмены — abort-guard:
/// при ЛЮБОМ выходе из функции (включая отмену родителя) обе задачи абортятся,
/// половинки устройства дропаются → TUN-fd закрывается → система убирает VPN.
/// (Голые spawn'ы без guard'а когда-то утекали и держали fd — «VPN висит,
/// а интернета нет»; guard сохраняет семантику инлайна.) На выходе шлём пиру
/// прощание (`close`), чтобы хост мгновенно освободил слот.
pub async fn run_guest<D>(device: D, link: Arc<dyn Link>) -> Result<()>
where
    D: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut reader, mut writer) = tokio::io::split(device);

    // интерфейс → канал
    let up_link = link.clone();
    let mut up = tokio::spawn(async move {
        let mut buf = vec![0u8; 65535];
        loop {
            let n = reader.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            up_link.send(&buf[..n]).await?;
        }
        Ok::<(), Error>(())
    });

    // канал → интерфейс. recv_into в ПЕРЕИСПОЛЬЗУЕМЫЙ буфер → нет аллокации на
    // каждый пакет на приёме (важно для CPU/батареи телефона-гостя).
    let down_link = link.clone();
    let mut down = tokio::spawn(async move {
        let mut buf = Vec::new();
        loop {
            if !down_link.recv_into(&mut buf).await? {
                break; // канал закрыт
            }
            if !from_host_allowed(&buf) {
                // Хост шлёт не нам — в стек это не отдаём (см. from_host_allowed).
                tracing::debug!("от хоста пакет не нашему адресу ({} Б) — отброшен", buf.len());
                continue;
            }
            writer.write_all(&buf).await?;
        }
        Ok::<(), Error>(())
    });

    // Абортит обе задачи при любом выходе/дропе — задачи НЕ переживают функцию.
    struct AbortBoth(tokio::task::AbortHandle, tokio::task::AbortHandle);
    impl Drop for AbortBoth {
        fn drop(&mut self) {
            self.0.abort();
            self.1.abort();
        }
    }
    let _guard = AbortBoth(up.abort_handle(), down.abort_handle());

    tokio::select! {
        _ = &mut up => {}
        _ = &mut down => {}
    }
    let _ = link.close().await; // прощаемся — хост освобождает слот сразу
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Фильтр знает адрес туннеля константой (горячий путь — строить TunParams на
    /// каждый пакет нельзя). Если адрес интерфейса когда-нибудь поменяют, фильтр
    /// начнёт резать ВСЁ и VPN тихо перестанет работать — тест это ловит сразу.
    #[test]
    fn guard_address_matches_tun_params() {
        assert_eq!(TunParams::guest().address.octets(), GUEST_TUN_ADDR);
    }

    /// Хост не должен доставать через туннель ни петлю гостя, ни его домашнюю сеть.
    #[test]
    fn host_cannot_reach_guest_localhost_or_lan() {
        // IPv4-заголовок: версия+IHL, потом всё нули до адресов (16..20 — получатель).
        let pkt = |dst: [u8; 4]| {
            let mut p = vec![0u8; 20];
            p[0] = 0x45;
            p[16..20].copy_from_slice(&dst);
            p
        };
        assert!(from_host_allowed(&pkt([10, 7, 0, 2])), "ответ нам обязан проходить");

        assert!(!from_host_allowed(&pkt([127, 0, 0, 1])), "петля гостя — службы без пароля");
        assert!(!from_host_allowed(&pkt([192, 168, 1, 50])), "домашний адрес гостя");
        assert!(!from_host_allowed(&pkt([192, 168, 1, 1])), "роутер гостя");
        assert!(!from_host_allowed(&pkt([10, 7, 0, 3])), "чужой адрес внутри туннеля");
        assert!(!from_host_allowed(&pkt([8, 8, 8, 8])), "транзит наружу через гостя");

        // Обрезки и IPv6 разбирать нечем — не пускаем (и не паникуем на срезе).
        assert!(!from_host_allowed(&[]));
        assert!(!from_host_allowed(&[0x45; 19]));
        let mut v6 = vec![0u8; 40];
        v6[0] = 0x60;
        assert!(!from_host_allowed(&v6), "IPv6 в туннеле не бывает — только маскировка чужого пакета");
    }
}
