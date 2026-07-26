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
