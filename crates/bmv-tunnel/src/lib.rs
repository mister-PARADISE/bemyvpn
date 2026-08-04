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

pub use host::{punch_target_allowed, run_host};

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

/// Пускать ли пакет, ПРОЧИТАННЫЙ С ИНТЕРФЕЙСА ГОСТЯ, в канал до хоста.
///
/// Зеркало `from_host_allowed`, и нужно оно из-за IPv6. Чтобы v6-трафик не
/// утекал мимо туннеля, платформа-оболочка обязана ЗАБРАТЬ IPv6 себе — то есть
/// объявить маршрут `::/0` на наш интерфейс (на iOS и Android иначе не умеют:
/// маршруты там ставит система, а не мы). После этого v6-пакеты приходят СЮДА —
/// и вот тут они обязаны кончиться:
///
///   * донести их некуда: обратный путь режет `from_host_allowed` (у туннеля
///     нет v6-адреса), значит ответ до гостя всё равно не дойдёт;
///   * отправить их хосту — значит ОТДАТЬ ЕМУ РОВНО ТОТ ТРАФИК, который человек
///     прятал: хост открыл бы наружу настоящие v6-сокеты и увидел все адреса.
///
/// Поэтому здесь проходит только IPv4. Это НЕ замена блокировке маршрутами: без
/// объявленного маршрута v6-пакет в интерфейс вообще не попадёт и уйдёт мимо —
/// фильтр лишь гарантирует, что попавший сюда пакет дальше не уедет.
fn to_host_allowed(pkt: &[u8]) -> bool {
    pkt.len() >= 20 && pkt[0] >> 4 == 4
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
            if !to_host_allowed(&buf[..n]) {
                continue; // IPv6/обрезок — хосту не отдаём (см. to_host_allowed)
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

    /// IPv6 ОТ ГОСТЯ НЕ УХОДИТ ХОСТУ НИ ПРИ КАКОМ ПОВЕДЕНИИ ОБОЛОЧКИ.
    ///
    /// Чтобы IPv6 не утекал мимо туннеля, оболочка забирает его себе маршрутом
    /// `::/0` — и тогда v6-пакеты валятся в наш интерфейс. Донести их некуда
    /// (обратный путь режет `from_host_allowed`), а отправить хосту — значит
    /// отдать ему ровно тот трафик, который человек прятал: хост открыл бы
    /// наружу настоящие v6-сокеты со своей стороны и увидел бы все адреса.
    #[test]
    fn ipv6_from_the_guest_never_reaches_the_host() {
        // Минимальный IPv6-заголовок — 40 байт, версия в старшем ниббле.
        let mut v6 = vec![0u8; 40];
        v6[0] = 0x60;
        assert!(!to_host_allowed(&v6), "IPv6-пакет ушёл бы хосту — это и есть утечка");
        // Длина тут ни при чём: режет именно версия.
        let mut big_v6 = vec![0u8; 1400];
        big_v6[0] = 0x6f;
        assert!(!to_host_allowed(&big_v6));

        // Обычный IPv4 обязан проходить — иначе VPN просто не работает.
        let mut v4 = vec![0u8; 20];
        v4[0] = 0x45;
        assert!(to_host_allowed(&v4));
        // Заголовок с опциями (IHL до 15) — тоже.
        let mut v4_opt = vec![0u8; 60];
        v4_opt[0] = 0x4f;
        assert!(to_host_allowed(&v4_opt));

        // Мусор и обрезки не должны ни проходить, ни ронять процесс по срезу.
        for n in 0..24usize {
            let mut p = vec![0u8; n];
            if n > 0 {
                p[0] = 0x45;
            }
            assert_eq!(to_host_allowed(&p), n >= 20, "длина {n}");
        }
        for ver in 0u8..16 {
            let mut p = vec![0u8; 40];
            p[0] = ver << 4;
            assert_eq!(to_host_allowed(&p), ver == 4, "версия {ver} прошла фильтр");
        }
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

    /// КАЖДЫЙ байт адреса обязан проверяться. Сдвинься срез на единицу — и через
    /// туннель прошло бы всё подсемейство 10.7.0.x, включая широковещательный
    /// 10.7.0.255, а тест выше этого бы не заметил.
    #[test]
    fn every_octet_of_destination_is_checked() {
        let pkt = |dst: [u8; 4]| {
            let mut p = vec![0u8; 20];
            p[0] = 0x45;
            p[16..20].copy_from_slice(&dst);
            p
        };
        let ok = [10u8, 7, 0, 2];
        assert!(from_host_allowed(&pkt(ok)));
        for i in 0..4 {
            let mut near = ok;
            near[i] = near[i].wrapping_add(1);
            assert!(!from_host_allowed(&pkt(near)), "сосед по байту {i} ({near:?}) прошёл фильтр");
            let mut near = ok;
            near[i] = near[i].wrapping_sub(1);
            assert!(!from_host_allowed(&pkt(near)), "сосед по байту {i} ({near:?}) прошёл фильтр");
        }
        assert!(!from_host_allowed(&pkt([10, 7, 0, 255])), "широковещательный внутри туннеля прошёл");
        assert!(!from_host_allowed(&pkt([10, 7, 0, 0])), "адрес сети прошёл");
    }

    /// Заголовок IPv4 бывает ДЛИННЕЕ 20 байт (опции, IHL до 15). Адрес получателя
    /// при этом остаётся на том же месте — фильтр обязан работать и с опциями,
    /// иначе хост обходил бы его, просто добавив к пакету опцию.
    #[test]
    fn header_options_do_not_bypass_the_filter() {
        for ihl in 5u8..=15 {
            let len = ihl as usize * 4;
            let mut p = vec![0u8; len];
            p[0] = 0x40 | ihl;
            p[16..20].copy_from_slice(&[10, 7, 0, 2]);
            assert!(from_host_allowed(&p), "IHL={ihl}: наш ответ зря отклонён");

            let mut bad = p.clone();
            bad[16..20].copy_from_slice(&[192, 168, 1, 1]);
            assert!(!from_host_allowed(&bad), "IHL={ihl}: чужой адрес прошёл");
        }
    }

    // ── Фильтры ПРИМЕНЕНЫ, а не просто написаны ──────────────────────────────
    //
    // Всё, что выше, зовёт чистые функции. Ни один из этих тестов не заметит,
    // если из `run_guest` убрать сам вызов: обе дыры (утечка IPv6 хосту и чужой
    // пакет в стек гостя) откроются заново при зелёном прогоне. Ниже — настоящая
    // качалка с настоящим каналом.

    /// Пакет IPv4 к указанному адресату (20 байт — заголовок без данных).
    fn v4_to(dst: [u8; 4]) -> Vec<u8> {
        let mut p = vec![0u8; 20];
        p[0] = 0x45;
        p[16..20].copy_from_slice(&dst);
        p
    }

    /// АПЛОАД: IPv6 из интерфейса гостя в канал НЕ УХОДИТ. Следом шлём законный
    /// IPv4 — хост обязан получить первым именно его.
    #[tokio::test]
    async fn the_upload_path_really_calls_the_filter() {
        use tokio::io::AsyncWriteExt;
        let (mut device, guest_end) = tokio::io::duplex(4096);
        let (guest_link, host_end) = bmv_common::wire::memory_pair(64);
        let _session = tokio::spawn(run_guest(guest_end, Arc::from(guest_link)));

        let mut v6 = vec![0u8; 40];
        v6[0] = 0x60;
        device.write_all(&v6).await.unwrap();
        // Пауза обязательна: `duplex` — поток, две записи подряд слились бы в одно
        // чтение, и тогда проверялась бы склейка, а не отдельный пакет.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        device.write_all(&v4_to([8, 8, 8, 8])).await.unwrap();

        let mut buf = Vec::new();
        let got = tokio::time::timeout(std::time::Duration::from_secs(2), host_end.recv_into(&mut buf))
            .await
            .expect("хост не получил вообще ничего — качалка встала")
            .unwrap();
        assert!(got, "канал закрылся вместо доставки пакета");
        assert_eq!(buf[0] >> 4, 4, "хосту уехал IPv6-пакет — ровно тот трафик, который человек прятал");
    }

    /// ДАУНЛОАД: пакет не нашему адресу в стек гостя НЕ ПОПАДАЕТ. Следом —
    /// законный ответ, он и обязан прийти первым.
    #[tokio::test]
    async fn the_download_path_really_calls_the_filter() {
        use tokio::io::AsyncReadExt;
        let (mut device, guest_end) = tokio::io::duplex(4096);
        let (guest_link, host_end) = bmv_common::wire::memory_pair(64);
        let _session = tokio::spawn(run_guest(guest_end, Arc::from(guest_link)));

        host_end.send(&v4_to([192, 168, 1, 1])).await.unwrap(); // домашний адрес гостя
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        host_end.send(&v4_to([10, 7, 0, 2])).await.unwrap(); // наш адрес в туннеле

        let mut buf = [0u8; 128];
        let n = tokio::time::timeout(std::time::Duration::from_secs(2), device.read(&mut buf))
            .await
            .expect("в интерфейс гостя не пришло ничего — качалка встала")
            .unwrap();
        assert!(n >= 20, "огрызок вместо пакета ({n} Б)");
        assert_eq!(&buf[16..20], &[10, 7, 0, 2], "в стек гостя попал пакет НЕ ЕГО адресу — хост шлёт мимо туннеля");
    }

    /// Граница длины: 19 байт разбирать нечем, 20 — ровно заголовок без данных.
    /// Проверяем обе стороны, потому что ошибка на единицу здесь — это паника
    /// по срезу от пакета, присланного чужой машиной.
    #[test]
    fn length_boundary_is_exact_and_never_panics() {
        for n in 0..64usize {
            let mut p = vec![0u8; n];
            if n > 0 {
                p[0] = 0x45;
            }
            if n >= 20 {
                p[16..20].copy_from_slice(&[10, 7, 0, 2]);
            }
            let allowed = from_host_allowed(&p); // главное — не паникует
            assert_eq!(allowed, n >= 20, "длина {n}: решение {allowed}, а ожидалось {}", n >= 20);
        }
    }
}
