//! Гостевая половина десктопного VPN: перебрать кандидатов, поднять туннель,
//! ОБЯЗАТЕЛЬНО попрощаться (BYE) и откатить маршруты.
//!
//! Жил внутри бинаря bmv-gui (helper.rs), поэтому терминальная оболочка взять
//! его не могла и написала свою упрощённую версию — без бюджетов, без перебора
//! и без BYE (хост держал ушедшего гостя лишние 8 секунд, до keepalive-таймаута).
//! Теперь код один на обе оболочки: чинится в одном месте — чинится везде.

use std::sync::Arc;
use std::time::Duration;

use bmv_config::Config;
use bmv_core::BmvEngine;
use bmv_signal::HostInfo;

/// Сколько лучших хостов реально перебирать по «Старту».
///
/// Раньше в перебор уходил ВЕСЬ каталог: при сотне свободных хостов это минуты,
/// и человек всё это время смотрит на «подключаюсь». Не подошёл никто из первой
/// пятёрки — проблема уже не в хостах.
pub const QUICK_MAX: usize = 5;

/// Порядок перебора для умного «Старта»: сначала ЧУЖАЯ страна, потом — где
/// свободнее. Возвращает не больше `QUICK_MAX` кандидатов, уже отсортированных.
///
/// Чужая страна раньше своей потому, что ради неё VPN и включают: хост в своей
/// же стране не меняет ничего, кроме лишнего плеча. Свободные места — второй
/// критерий: подключаться к хосту, у которого занято 7 из 8, значит делить
/// канал с семерыми.
///
/// `cc_of` передаётся снаружи, потому что страну оболочки узнают по-разному:
/// у окна есть встроенная база IP→страна, у терминала её нет (3.4 МБ в
/// серверный бинарь ради имени страны не тащим) и остаётся поле из анонса.
pub fn rank_candidates(
    hosts: &[HostInfo],
    my_cc: Option<&str>,
    own_id: &str,
    cc_of: &dyn Fn(&HostInfo) -> Option<String>,
) -> Vec<HostInfo> {
    let mut cands: Vec<HostInfo> = hosts
        .iter()
        // Хост с паролем в «Старт» не годится: пароля у нас нет, а спрашивать
        // его у человека — это уже не «одна кнопка».
        .filter(|h| h.online && !h.has_password && h.guests < h.max_guests && h.id != own_id)
        .cloned()
        .collect();
    cands.sort_by(|a, b| {
        if let Some(m) = my_cc {
            let (af, bf) = (cc_of(a).as_deref() != Some(m), cc_of(b).as_deref() != Some(m));
            if af != bf {
                return bf.cmp(&af); // чужая страна (true) идёт первой
            }
        }
        // saturating: фильтр выше уже гарантирует guests < max_guests, но
        // вычитание в u32 не то место, где стоит полагаться на инвариант.
        b.max_guests.saturating_sub(b.guests).cmp(&a.max_guests.saturating_sub(a.guests))
    });
    cands.truncate(QUICK_MAX);
    cands
}

/// Что сообщать наружу. Раньше здесь ходили готовые строки `STATE\t…` протокола
/// root-хелпера — но это формат ОДНОЙ оболочки, и терминалу пришлось бы их
/// разбирать обратно. Оболочка сама решает, как показать состояние.
#[derive(Debug, Clone, PartialEq)]
pub enum State {
    /// Идёт попытка к этому хосту (id).
    Connecting(String),
    /// Туннель поднят, весь трафик уже идёт через этот хост (id).
    Up(String),
    /// Туннеля нет (штатная остановка либо обрыв).
    Off,
    /// ХОСТ САМ ПОГАСИЛ РАЗДАЧУ (прислал BYE) — сеанс окончен, и это НЕ ошибка.
    ///
    /// Отдельно от `Off` и от `Failed`, потому что человеку тут нужно разное:
    /// после «Стоп» он и так знает, что выключил VPN сам; после отказа — что
    /// подключиться не вышло; а здесь всё сработало правильно, просто хост ушёл,
    /// и надо предложить выбрать другой. Со сплошным `Off` конец раздачи выглядел
    /// как беспричинно погасший VPN.
    HostLeft,
    /// Не получилось; текст — для человека.
    Failed(String),
}

/// Поводок для кандидата, ПОСЛЕ которого есть кого попробовать ещё. Внутри
/// `guest_establish` на пробивание NAT отведено 12с — щедро, потому что двум
/// мобильным NAT столько и нужно. Но когда в очереди ждут живые хосты, сидеть
/// эти 12с у молчащего смысла нет: дешевле взять следующего. 5с покрывают
/// запрос к координатору (доли секунды по уже открытому сокету) плюс несколько
/// раундов пробивания — обычная пара NAT укладывается за секунду-две.
pub const TRY_NEXT_AFTER: Duration = Duration::from_secs(5);
/// Поводок для ПОСЛЕДНЕЙ попытки — запасных больше нет, поэтому даём пробиванию
/// доработать полностью (12с) с запасом на подготовку. Без этого у человека за
/// строгим NAT умная кнопка не срабатывала бы НИКОГДА, сколько ни жми.
pub const LAST_CHANCE: Duration = Duration::from_secs(15);

/// Бюджет на попытку номер `i` из `total`.
///
/// Вынесено отдельной функцией ради теста: на живом туннеле разницу между
/// «переберёт всех» и «встанет на первом молчащем» видно только по секундомеру.
pub fn budget(i: usize, total: usize) -> Duration {
    if i + 1 >= total { LAST_CHANCE } else { TRY_NEXT_AFTER }
}

/// Перебрать кандидатов: первый, к кому удалось подключиться, — запускаем туннель.
/// Порядок задаёт вызывающий (лучшие первыми), поводок — `budget`. Умный «Старт»
/// на iOS перебирает так же.
/// `cfg` — конфиг гостя ЦЕЛИКОМ, а не только адрес координатора: в нём живут
/// список STUN-серверов и настройки протоколов, и подставлять вместо них
/// умолчания значило бы тихо игнорировать то, что человек прописал в файле.
pub async fn run_candidates<F>(
    cfg: Config,
    cands: Vec<(String, String, String)>, // (host, pw, proto)
    on_state: F,
    stop: Arc<tokio::sync::Notify>,
) where
    F: Fn(State) + Send + Sync,
{
    // Режим IPv6 достаём ДО того, как конфиг уедет в движок: заворачивание
    // трафика — забота этого слоя, движок про маршруты не знает.
    let ipv6 = cfg.guest.ipv6_mode();
    let eng = Arc::new(BmvEngine::from_config(cfg));

    let total = cands.len();
    for (i, (host, pw, proto)) in cands.into_iter().enumerate() {
        on_state(State::Connecting(host.clone()));
        let pw_opt = (!pw.is_empty()).then_some(pw.clone());
        let proto_opt = (!proto.is_empty()).then_some(proto.clone());
        let est_fut = tokio::time::timeout(
            budget(i, total),
            eng.guest_establish(&host, pw_opt.as_deref(), proto_opt.as_deref()),
        );
        tokio::pin!(est_fut);
        let est = tokio::select! {
            _ = stop.notified() => {
                // Отключились ВО ВРЕМЯ подключения: даём establish короткую фору —
                // если link успел подняться, шлём BYE (хост мог уже завести сессию).
                if let Ok(Ok(Ok((_p, link)))) = tokio::time::timeout(Duration::from_secs(2), &mut est_fut).await {
                    let _ = tokio::time::timeout(Duration::from_millis(600), link.close()).await;
                }
                on_state(State::Off);
                return;
            }
            r = &mut est_fut => r,
        };
        if let Ok(Ok((peer, link))) = est {
            run_tunnel(&host, peer, link, eng.peer_check(), ipv6, &on_state, &stop).await;
            return;
        }
    }
    on_state(State::Failed("не удалось подключиться".into()));
}

async fn run_tunnel<F>(
    host: &str,
    peer: std::net::SocketAddr,
    link: Box<dyn bmv_common::Link>,
    hints: Option<tokio::sync::watch::Receiver<u64>>,
    ipv6: bmv_config::Ipv6Mode,
    on_state: &F,
    stop: &Arc<tokio::sync::Notify>,
) where
    F: Fn(State) + Send + Sync,
{
    let params = bmv_tunnel::TunParams::guest();
    let (device, ifname) = match crate::make_tun(&params) {
        Ok(d) => d,
        Err(e) => return on_state(State::Failed(format!("TUN: {e}"))),
    };
    let _guard = match crate::RouteGuard::install_with(peer.ip(), &ifname, ipv6) {
        Ok(g) => g,
        Err(e) => return on_state(State::Failed(format!("маршрут: {e}"))),
    };
    on_state(State::Up(host.to_string()));
    let link_arc: Arc<dyn bmv_common::Link> = Arc::from(link);
    // Кто завершил сеанс? Прощание хоста читаем ЗДЕСЬ, до `close()`: дальше свой
    // BYE шлём уже мы, и разбираться, чей он был, поздно.
    let host_left = tokio::select! {
        _ = bmv_tunnel::run_guest(device, link_arc.clone()) => link_arc.peer_said_bye(),
        _ = stop.notified() => false,
        // Подсказки координатора «проверь соседа» — на всё время туннеля: хост
        // мог быть убит, и прощание по UDP послать было некому. Ретранслятор сам
        // не завершается, ветка нужна только чтобы он крутился рядом.
        _ = BmvEngine::relay_peer_checks(&*link_arc, hints) => false,
    };
    // ВСЕГДА прощаемся (BYE) — и при «Стоп», и если run_guest завершился сам
    // (обрыв/ошибка). Хост увидит EOF сразу и снимет гостя из счётчика, не ожидая
    // keepalive-таймаута (8с). BYE идёт ДО снятия RouteGuard (_guard жив до конца).
    let _ = tokio::time::timeout(Duration::from_millis(600), link_arc.close()).await;
    // guard снимется здесь (Drop) — маршруты откатятся
    on_state(if host_left { State::HostLeft } else { State::Off });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Последнему кандидату даётся ПОЛНЫЙ срок, остальным — короткий поводок.
    ///
    /// Регрессия сразу на две поломки, которые уже случались: с одним общим
    /// коротким сроком человек за строгим NAT не подключался НИКОГДА (12с
    /// пробивания не помещались в 5с), а с одним общим длинным перебор пяти
    /// хостов занимал минуту, и всё это время на экране висело «подключаюсь».
    #[test]
    fn last_candidate_gets_the_full_budget_and_the_others_a_short_leash() {
        // Единственный кандидат — он же и последний: полный срок.
        assert_eq!(budget(0, 1), LAST_CHANCE);
        // Пятеро: четверым поводок, пятому полный срок.
        for i in 0..4 {
            assert_eq!(budget(i, 5), TRY_NEXT_AFTER, "кандидат {i} из 5");
        }
        assert_eq!(budget(4, 5), LAST_CHANCE);
        // Полный срок обязан перекрывать 12с окна пробивания NAT, иначе строгий
        // NAT не пробивается вообще.
        assert!(LAST_CHANCE >= Duration::from_secs(12));
        assert!(TRY_NEXT_AFTER < LAST_CHANCE);
    }

    fn host(id: &str, cc: &str, guests: u32, max: u32) -> HostInfo {
        HostInfo {
            id: id.into(),
            country: cc.into(),
            online: true,
            guests,
            max_guests: max,
            ..HostInfo::default()
        }
    }
    /// Страна как её видит терминал — из поля анонса.
    fn by_country(h: &HostInfo) -> Option<String> {
        (!h.country.is_empty()).then(|| h.country.clone())
    }

    /// «Старт» обязан вести в ЧУЖУЮ страну, а среди равных — туда, где свободнее.
    #[test]
    fn quick_start_prefers_a_foreign_country_and_then_free_room() {
        let hosts = vec![
            host("SVOY", "RU", 0, 100), // своя страна, зато совсем пустой
            host("TESNO", "NL", 7, 8),  // чужая, но почти забита
            host("PROSTOR", "DE", 1, 50), // чужая и просторная
        ];
        let order: Vec<String> = rank_candidates(&hosts, Some("RU"), "", &by_country)
            .iter()
            .map(|h| h.id.clone())
            .collect();
        assert_eq!(order, ["PROSTOR", "TESNO", "SVOY"], "своя страна обязана быть последней");

        // Страна своя неизвестна — остаётся один критерий, свободные места.
        let order: Vec<String> = rank_candidates(&hosts, None, "", &by_country)
            .iter()
            .map(|h| h.id.clone())
            .collect();
        assert_eq!(order, ["SVOY", "PROSTOR", "TESNO"]);
    }

    /// Кого «Старт» не должен предлагать вообще.
    #[test]
    fn quick_start_skips_the_useless_and_ones_own_host() {
        let mut offline = host("OFF", "NL", 0, 8);
        offline.online = false;
        let mut locked = host("LOCK", "NL", 0, 8);
        locked.has_password = true;
        let hosts = vec![
            offline,
            locked,
            host("FULL", "NL", 8, 8), // мест нет
            host("MOY", "NL", 0, 8),  // собственная раздача — петля на себя
            host("OK", "NL", 0, 8),
        ];
        let picked: Vec<String> =
            rank_candidates(&hosts, Some("RU"), "MOY", &by_country).iter().map(|h| h.id.clone()).collect();
        assert_eq!(picked, ["OK"]);
    }

    /// Очередь ограничена: сотня свободных хостов не превращается в минуту перебора.
    #[test]
    fn the_queue_never_grows_past_a_handful() {
        let hosts: Vec<HostInfo> = (0..100).map(|i| host(&format!("H{i}"), "NL", 0, 8)).collect();
        assert_eq!(rank_candidates(&hosts, Some("RU"), "", &by_country).len(), QUICK_MAX);
        // …и на длину очереди опирается расчёт бюджетов: полный срок получает
        // ровно последний из них.
        assert_eq!(budget(QUICK_MAX - 1, QUICK_MAX), LAST_CHANCE);
    }
}
