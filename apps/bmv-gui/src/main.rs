//! Десктопный GUI BeMyVPN на Slint — построчная копия iOS-приложения (тот же вид,
//! те же анимации, тот же поток подключения). Ядро `bmv-core` линкуется НАПРЯМУЮ
//! (без FFI/JVM/webview), роутинг гостя — общий крейт `bmv-desktop`. Один
//! самодостаточный бинарь под Windows/Linux/macOS: Slint компилит один и тот же
//! .slint-код нативно под каждую ОС.
//!
//! Мост UI↔ядро: Slint крутит event-loop на главном потоке, ядро async — на
//! tokio. Обновления из ядра в UI идут через `invoke_from_event_loop`.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bmv_config::Config;
use bmv_core::BmvEngine;
use bmv_signal::HostInfo;
use slint::{Model, ModelRc, SharedString, VecModel, Weak};

mod flags;
mod geo;
mod helper;
mod store;

slint::include_modules!();

const DEFAULT_COORD: &str = "https://bemyvpn.net";

type EngineSlot = Arc<Mutex<Arc<BmvEngine>>>;
type Ts = Arc<Mutex<Option<std::time::Instant>>>;
/// Замеры отклика: id хоста → готовая строка для плитки («24 мс» / «—» / «…»).
/// Живёт рядом с каталогом, потому что строки каталога пересобираются на каждом
/// обновлении, а измерение переживать это обязано — иначе цифра мигала бы.
/// Замеры отклика: id хоста → Some(мс) / None (не ответил). Отсутствие ключа =
/// ещё не мерили. Храним ЧИСЛО, а текст и цвет строятся из него — иначе цвет
/// зависел бы от того, как мы подписали значение.
type Pings = Arc<Mutex<std::collections::HashMap<String, Option<u32>>>>;

/// Часы сессии: MM:SS, после часа H:MM:SS (как uptimeText на iOS).
fn uptime_text(sec: u64) -> String {
    let (h, m, s) = (sec / 3600, (sec % 3600) / 60, sec % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m:02}:{s:02}") }
}

/// Имя протокола по-человечески (protoName на iOS).
fn proto_name(p: &str) -> &str {
    match p {
        "noise" | "noise-aes" => "Обычный",
        "noise-obfs" => "Маскировка",
        _ => "Без шифра",
    }
}

fn display_name(h: &HostInfo) -> String {
    if h.name.is_empty() { h.id.clone() } else { h.name.clone() }
}

/// Код страны хоста: сперва локально по IP (как iOS), фолбэк — announce-поле.
fn host_cc(h: &HostInfo) -> Option<String> {
    geo::country_of(&h.ip).or_else(|| {
        let c = h.country.trim();
        (c.len() == 2 && c.chars().all(|ch| ch.is_ascii_alphabetic())).then(|| c.to_ascii_uppercase())
    })
}

/// Скачать релиз, сверить sha256 и запустить установку.
///
/// Возврат Ok означает «помощник запущен» — приложение после этого обязано
/// завершиться, иначе подмена не пройдёт: пока процесс жив, файлы держатся.
async fn fetch_update(tag: &str) -> Result<(), String> {
    let asset = bmv_common::update::current_asset_name(true)
        .ok_or("для этой платформы релизы не выпускаются")?;
    let repo = std::env::var("BMV_REPO").unwrap_or_else(|_| "mister-PARADISE/bemyvpn".into());
    let url = bmv_common::update::asset_url(&repo, tag, asset);

    // Целостность обеспечивает HTTPS: файл идёт с github.com, подменить его в
    // пути нельзя. Отдельная проверка хэша тут ничего бы не добавила.
    let bytes = bmv_common::update::download(&url, bmv_common::update::MAX_ASSET_BYTES)
        .await
        // Частый случай: GitHub заблокирован. Наш же продукт это и решает.
        .map_err(|e| format!("{e} — попробуйте подключиться к VPN"))?;

    #[cfg(target_os = "macos")]
    bmv_common::update::spawn_bundle_updater(&bytes).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    bmv_common::update::spawn_exe_updater(&bytes).map_err(|e| e.to_string())?;
    // Linux: AppImage — один файл, подменяем как терминальный бинарь.
    #[cfg(all(unix, not(target_os = "macos")))]
    bmv_common::update::replace_self(&bytes).map(|_| ()).map_err(|e| e.to_string())?;
    Ok(())
}

/// Заполнить карточку активного подключения по записи хоста. Зовётся В МОМЕНТ
/// подключения (данные уже на руках — гость выбрал этот хост) и потом на каждом
/// обновлении каталога, чтобы живые цифры (гости) не отставали.
fn fill_vpn_card(ui: &AppWindow, h: &HostInfo) {
    ui.set_vpn_ip(if h.ip.is_empty() { "—".into() } else { h.ip.clone().into() });
    ui.set_vpn_country(host_cc(h).unwrap_or_else(|| "—".into()).into());
    ui.set_vpn_guests(format!("{} / {}", h.guests, h.max_guests).into());
    ui.set_vpn_proto(proto_name(&h.protocol).into());
    ui.set_vpn_proto_id(h.protocol.clone().into());
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Режим привилегированного туннель-хелпера (root): поднят самим приложением
    // через системный запрос пароля. Без окна — только качает туннель. Никогда
    // не возвращается. (См. helper.rs.) Только Unix: на Windows схема без отдельного
    // процесса — приложение целиком под админом (манифест UAC), туннель внутри.
    #[cfg(not(windows))]
    {
        let args: Vec<String> = std::env::args().collect();
        if args.len() >= 5 && args[1] == "--tunnel-helper" {
            helper::run_helper(&args[2], &args[3], &args[4]);
        }
    }

    // Windows: разрешить нашему exe входящий UDP в брандмауэре — иначе UDP-пробитие
    // к хосту не проходит и подключение виснет (мы уже под админом, манифест UAC).
    #[cfg(windows)]
    bmv_desktop::ensure_firewall_allow();

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let handle = rt.handle().clone();

    let config = Config::load(None).unwrap_or_default();
    let coord = config.coordinators.first().cloned().unwrap_or_else(|| DEFAULT_COORD.into());
    let engine: EngineSlot = Arc::new(Mutex::new(Arc::new(BmvEngine::from_config(config))));

    let ui = AppWindow::new()?;
    ui.set_coord_url(pretty_url(&coord).into());
    ui.set_coord_field(pretty_url(&coord).into());
    ui.set_coord_is_default(coord == DEFAULT_COORD);
    ui.set_vpn_status("VPN выключен".into());
    ui.set_host_status("Раздача выключена".into());
    ui.set_server_history(str_model(
        &store::load_server_history().iter().map(|u| pretty_url(u)).collect::<Vec<_>>(),
    ));

    // Недавние (для текущего координатора) — id хранятся, имена берём из каталога.
    let recent_ids: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(store::load_recent(&coord)));
    // Последний известный свой IP — для «умного Старта» (сначала чужая страна).
    let my_ip: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));

    // ПОЛНЫЙ адрес координатора — со схемой. Отдельно от `coord-url`, который
    // теперь показывается доменом: тот годится только для экрана. Полный нужен
    // и для сетевых запросов, и как ключ хранилища (недавние привязаны к
    // координатору). Смешивать их нельзя: «bemyvpn.net» и «https://bemyvpn.net»
    // — разные ключи и неразбираемый URL.
    let coord_full: Arc<Mutex<String>> = Arc::new(Mutex::new(coord.clone()));

    // Замеры отклика до хостов (заполняются по раскрытию карточки).
    let pings: Pings = Arc::new(Mutex::new(std::collections::HashMap::new()));

    let host_started: Ts = Arc::new(Mutex::new(None));
    let vpn_since: Ts = Arc::new(Mutex::new(None));

    // Файл-маркер «туннель поднят» от root-хелпера (надёжный канал для UI в обход
    // TCP-STATE, который на idle event-loop macOS до окна мог не дойти). Путь
    // привязан к pid GUI — и хелпер, и фон-цикл знают его одинаково.
    let up_file = std::env::temp_dir().join(format!("bemyvpn-up-{}", std::process::id()));
    let _ = std::fs::remove_file(&up_file);

    // Копирование в буфер (тап по плиткам Код/IP).
    ui.on_copy_text(|t| {
        if !t.is_empty() {
            if let Ok(mut cb) = arboard::Clipboard::new() {
                let _ = cb.set_text(t.to_string());
            }
        }
    });

    // «Обновить»: скачать, проверить, установить. Подпись манифеста проверена
    // ещё при получении, здесь остаётся sha256 файла. На десктопе установка идёт
    // через помощника: приложение не может подменить себя на ходу — оно выходит,
    // помощник доделывает.
    {
        let (weak, handle2) = (ui.as_weak(), handle.clone());
        ui.on_do_update(move || {
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_update_state() == 1 { return; } // уже качаем — второй раз не начинаем
            let tag = ui.get_update_tag().to_string();
            if tag.is_empty() { return; }
            ui.set_update_state(1);
            let weak2 = weak.clone();
            handle2.spawn(async move {
                let res = fetch_update(&tag).await;
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak2.upgrade() else { return };
                    match res {
                        Ok(()) => {
                            ui.set_update_state(2);
                            // ВЫХОДИМ САМИ. Помощник ждёт смерти этого процесса,
                            // чтобы подменить бандл (на Windows — файл .exe);
                            // пока мы живы, подменять нечего. Раньше приложение
                            // писало «перезапустите» и продолжало работать —
                            // человек окно не закрывал, помощник ждал полминуты
                            // и лез двигать бандл ПОД РАБОТАЮЩИМ приложением.
                            // На Linux этой беды не было: там AppImage меняется
                            // на месте, помощник не нужен.
                            //
                            // Секунда паузы — чтобы надпись «Готово» успели
                            // прочесть и закрытие окна не выглядело сбоем.
                            slint::Timer::single_shot(Duration::from_millis(1200), || {
                                let _ = slint::quit_event_loop();
                            });
                        }
                        Err(e) => {
                            ui.set_update_state(3);
                            ui.set_update_error(e.into());
                        }
                    }
                });
            });
        });
    }

    // QR-оверлей: Rust рисует картинку по коду и открывает оверлей.
    {
        let weak = ui.as_weak();
        ui.on_show_qr(move |code| {
            if let Some(ui) = weak.upgrade() {
                if let Some(img) = qr_image(&format!("bemyvpn://{code}")) {
                    ui.set_qr_overlay_img(img);
                }
                ui.set_qr_overlay(code);
            }
        });
    }

    // Проверка обновления — один раз при запуске, прямо у GitHub. Своей
    // инфраструктуры для этого не нужно: релизы и так лежат там.
    {
        let weak = ui.as_weak();
        handle.spawn(async move {
            if !bmv_common::version::is_release_build() {
                return; // локальную сборку обновлять незачем
            }
            let repo = std::env::var("BMV_REPO").unwrap_or_else(|_| "mister-PARADISE/bemyvpn".into());
            let Ok(tag) = bmv_common::update::github_latest_tag(&repo).await else { return };
            let latest = tag.trim_start_matches('v').to_string();
            if !bmv_common::version::is_newer(&latest, bmv_common::version::VERSION) {
                return;
            }
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak.upgrade() {
                    ui.set_update_version(latest.into());
                    ui.set_update_tag(tag.into());
                }
            });
        });
    }

    let hosts: Arc<Mutex<Vec<HostInfo>>> = Arc::new(Mutex::new(Vec::new()));
    let host_task: Rc<RefCell<Option<tokio::task::JoinHandle<()>>>> = Rc::new(RefCell::new(None));

    // Колбэк статусов от root-хелпера (крутится в потоке-читателе) → UI. Имя хоста
    // достаём из живого каталога по id; недавние пишем по факту подключения.
    let on_state: Arc<dyn Fn(i32, String, String) + Send + Sync> = {
        let (hosts, recent, weak) = (hosts.clone(), recent_ids.clone(), ui.as_weak());
        let vpn_since = vpn_since.clone();
        let coord_full = coord_full.clone();
        Arc::new(move |n: i32, id: String, err: String| {
            // Часы сессии: тикают с момента реального поднятия туннеля.
            {
                let mut vs = vpn_since.lock().unwrap();
                match n {
                    2 => { if vs.is_none() { *vs = Some(std::time::Instant::now()); } }
                    1 => {}
                    _ => *vs = None,
                }
            }
            let (hosts, recent, weak) = (hosts.clone(), recent.clone(), weak.clone());
            let coord_full = coord_full.clone();
            let _ = slint::invoke_from_event_loop(move || {
                let Some(ui) = weak.upgrade() else { return };
                let name = hosts.lock().unwrap().iter().find(|h| h.id == id).map(display_name).unwrap_or_else(|| id.clone());
                match n {
                    1 => { ui.set_vpn_state(1); ui.set_vpn_host_id(id.clone().into()); ui.set_vpn_status("Подключаюсь…".into()); ui.set_vpn_sub(format!("к {name}").into()); }
                    2 => {
                        ui.set_vpn_state(2); ui.set_vpn_host_id(id.clone().into()); ui.set_vpn_status(name.into());
                        // Кто именно подключился, знает хелпер: при «Старте» он сам
                        // выбирает хост из списка кандидатов, и до этого момента id
                        // нам неизвестен. Заполняем карточку здесь — иначе ячейки
                        // пустуют до ответа каталога.
                        if let Some(h) = hosts.lock().unwrap().iter().find(|h| h.id == id) {
                            fill_vpn_card(&ui, h);
                        }
                        { let mut r = recent.lock().unwrap(); r.retain(|x| x != &id); r.insert(0, id.clone()); r.truncate(6); }
                        let _ = store::add_recent(&coord_full.lock().unwrap(), &id);
                    }
                    3 => { ui.set_vpn_state(3); ui.set_vpn_status(if err.is_empty() { "не удалось подключиться".into() } else { err.clone().into() }); ui.set_vpn_sub(err.into()); }
                    _ => { ui.set_vpn_state(0); ui.set_vpn_host_id("".into()); ui.set_vpn_status("VPN выключен".into()); ui.set_vpn_sub("".into()); }
                }
            });
        })
    };
    let hlp = Rc::new(helper::Helper::new(on_state, up_file.clone()));
    // Копия для аккуратного гашения при закрытии окна (Windows: туннель в этом же
    // процессе, поэтому откат маршрутов надо инициировать до выхода — см. ниже).
    #[cfg(windows)]
    let hlp_shutdown = hlp.clone();

    // Код хоста запрашиваем сразу (как ensureHostCode на iOS onAppear) — чтобы во
    // вкладке «Хост» он был виден, не дожидаясь «Стать хостом».
    ensure_host_code(&ui, &engine, &handle);

    spawn_refresh(engine.clone(), hosts.clone(), ui.as_weak(), &handle,
                  host_started.clone(), vpn_since.clone(), recent_ids.clone(), my_ip.clone(), up_file.clone(), pings.clone());
    let hosts_for_probe = hosts.clone();
    // Копия для имени хоста по умолчанию: сам my_ip уходит в wire_vpn.
    let my_ip_for_host = my_ip.clone();
    wire_vpn(&ui, hosts, hlp, my_ip, coord_full.clone());
    wire_expand_probe(&ui, hosts_for_probe, engine.clone(), &handle, pings);
    wire_host(&ui, engine.clone(), host_task, handle.clone(), host_started, my_ip_for_host);
    wire_coord(&ui, engine, coord_full.clone());

    ui.run()?;
    let _ = std::fs::remove_file(&up_file); // убрать маркер (хелпер тоже уберёт при выходе)

    // Окно закрыто. На Windows туннель живёт в этом процессе, поэтому явно просим
    // откатить маршруты/DNS и даём мгновение отработать (иначе резкий выход
    // процесса мог бы оставить пин-маршрут и DNS; split-default Windows снимает
    // сам вместе с исчезновением wintun-адаптера).
    #[cfg(windows)]
    {
        hlp_shutdown.stop();
        std::thread::sleep(Duration::from_millis(500));
    }

    Ok(())
}

/// Запросить код хоста у сервера и показать (если ещё не сохранён локально).
fn ensure_host_code(ui: &AppWindow, engine: &EngineSlot, handle: &tokio::runtime::Handle) {
    let (id, sig) = store::load_host_creds();
    if !id.is_empty() && !sig.is_empty() {
        ui.set_host_code(id.into());
        return;
    }
    let eng = engine.lock().unwrap().clone();
    let weak = ui.as_weak();
    handle.spawn(async move {
        if let Ok((code, s)) = eng.host_new_code().await {
            if !code.is_empty() && !s.is_empty() {
                store::save_host_creds(&code, &s);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() { ui.set_host_code(code.into()); }
                });
            }
        }
    });
}

fn str_model(v: &[String]) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(v.iter().map(|s| s.clone().into()).collect::<Vec<SharedString>>()))
}

/// Имя хоста по умолчанию — СТРАНА по своему IP, а не имя компьютера.
///
/// Раньше здесь был `scutil --get ComputerName` (macOS) или `hostname`, и у людей
/// это обычно «MacBook Air — Armen»: настоящее имя владельца уезжало в ПУБЛИЧНЫЙ
/// каталог, который видят все. Проверено на живой машине — именно так и было.
/// Страна берётся офлайн из встроенной базы, наружу ничего не спрашиваем.
fn default_host_name(my_ip: &str) -> String {
    bmv_common::default_host_name(geo::country_of(my_ip).as_deref())
}


/// Живой замер отклика: пока карточка раскрыта, проба идёт РАЗ В СЕКУНДУ.
///
/// Ровно как watchPing на iOS и Android — цифра должна дышать, а не застывать на
/// первом значении. Отклик меняется вместе с сетью, и застывшее число врёт тем
/// хуже, чем дольше на него смотрят.
///
/// Почему только у раскрытой карточки, а не у всего списка: проба — настоящий
/// сетевой запрос к чужой машине. Гонять их пачкой ради строк, на которые никто
/// не смотрит, значит без нужды дёргать десятки хостов каждую секунду.
fn wire_expand_probe(
    ui: &AppWindow,
    hosts: Arc<Mutex<Vec<HostInfo>>>,
    engine: EngineSlot,
    handle: &tokio::runtime::Handle,
    pings: Pings,
) {
    let weak = ui.as_weak();
    let handle = handle.clone();
    // Номер живого замера. Каждое раскрытие/сворачивание его увеличивает, и цикл,
    // увидев чужой номер, выходит сам. Так на телефонах работает отмена задачи
    // (pingTask?.cancel() / pingJob?.cancel()) — здесь тот же смысл без лишних
    // сущностей: свернули карточку или открыли другую — прошлый цикл умер.
    let gen = Arc::new(std::sync::atomic::AtomicU64::new(0));

    ui.on_expanded_host(move |idx| {
        use std::sync::atomic::Ordering;
        // Увеличиваем ВСЕГДА — это и есть отмена предыдущего цикла.
        let mine = gen.fetch_add(1, Ordering::SeqCst) + 1;

        // -1 приходит при сворачивании: мерить больше нечего.
        if idx < 0 {
            return;
        }
        let Some(h) = hosts.lock().unwrap().get(idx as usize).cloned() else { return };

        // Без адресов пробить некуда — честное «не ответил» сразу, без цикла
        // вхолостую (тот же ранний выход, что на iOS и Android).
        if h.endpoints.is_empty() {
            pings.lock().unwrap().insert(h.id.clone(), None);
            let (weak2, id) = (weak.clone(), h.id.clone());
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak2.upgrade() {
                    set_row_ping(&ui, &id, None);
                }
            });
            return;
        }
        // Пока ключа нет, плитка показывает пульсацию «идёт замер».

        let eng = engine.lock().unwrap().clone();
        let (weak2, pings2, gen2) = (weak.clone(), pings.clone(), gen.clone());
        handle.spawn(async move {
            // Цикл, а не один замер. Раньше здесь стояла проверка «уже мерили —
            // выходим», и цифра застывала на первом значении до конца сессии,
            // хотя на телефонах она обновляется каждую секунду.
            while gen2.load(Ordering::SeqCst) == mine {
                // Честное «не ответил» (None): хост может быть за таким NAT, что
                // без пробивания до него не достучаться. Выдумывать число нельзя.
                let ms = eng.probe_host_rtt(&h.id, &h.endpoints).await;
                if gen2.load(Ordering::SeqCst) != mine {
                    return;
                }
                pings2.lock().unwrap().insert(h.id.clone(), ms);
                let (weak3, id) = (weak2.clone(), h.id.clone());
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak3.upgrade() {
                        set_row_ping(&ui, &id, ms);
                    }
                });
                // Пауза ПОСЛЕ замера, а не параллельно: у пробы свой срок, и запуск
                // нового замера поверх незакрытого копил бы их.
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });
    });
}

/// Вписать отклик в ОДНУ строку списка, не трогая остальные.
///
/// Перестроение всего списка схлопнуло бы раскрытую карточку прямо под руками —
/// а замер идёт как раз пока она открыта.
fn set_row_ping(ui: &AppWindow, id: &str, ms: Option<u32>) {
    let rows = ui.get_hosts();
    for i in 0..rows.row_count() {
        if let Some(mut r) = rows.row_data(i) {
            if r.id == id {
                r.ping = ms.map(|v| format!("{v} мс")).unwrap_or_else(|| "—".into()).into();
                r.ping_ms = ms.map(|v| v.min(i32::MAX as u32) as i32).unwrap_or(-1);
                rows.set_row_data(i, r);
                break;
            }
        }
    }
}

/// Строка каталога → карточка (host_row на iOS): подпись, полоса, значок протокола.
fn host_row(h: &HostInfo, pings: &Pings) -> HostRow {
    // ЛОК БЕРЁТСЯ ОДИН РАЗ И СРЕЗАЕТСЯ В ЛОКАЛЬНЫЕ.
    //
    // Раньше здесь было два `pings.lock()` — прямо в литерале структуры, по
    // одному на поле. Временные значения в выражении живут до конца ВСЕГО
    // оператора, поэтому первый guard был ещё жив, когда брался второй, а
    // `std::sync::Mutex` не реентрантный — самодедлок. И не где-нибудь, а на
    // потоке интерфейса: окно вставало намертво, ничего нельзя нажать, каталог
    // не наполнялся, и это читалось как «не подключается к серверу».
    //
    // Не воспроизводилось при ПУСТОМ каталоге: нет строк — нет вызова. Именно
    // поэтому сборка и запуск «у себя» ничего не показали.
    let (ping_text, ping_ms) = {
        let map = pings.lock().unwrap();
        match map.get(&h.id) {
            Some(Some(ms)) => (
                SharedString::from(format!("{ms} мс")),
                (*ms).min(i32::MAX as u32) as i32,
            ),
            Some(None) => (SharedString::from("—"), -1),
            None => (SharedString::new(), -1),
        }
    };
    let usable = h.online && h.guests < h.max_guests;
    let cc = host_cc(h);
    let flag_img = cc.as_deref().and_then(flags::flag);
    let sub = {
        let mut parts: Vec<String> = Vec::new();
        if let Some(c) = &cc { parts.push(c.clone()); } else if !h.ip.is_empty() { parts.push(h.ip.clone()); }
        parts.push(format!("гостей {}/{}", h.guests, h.max_guests));
        parts.join(" · ")
    };
    let fill = if h.max_guests > 0 { (h.guests as f32 / h.max_guests as f32).clamp(0.0, 1.0) } else { 0.0 };
    HostRow {
        id: h.id.clone().into(),
        name: display_name(h).into(),
        flag_img: flag_img.clone().unwrap_or_default(),
        has_flag: flag_img.is_some(),
        subtitle: sub.into(),
        obfs: (h.protocol == "noise-obfs"),
        locked: h.has_password,
        usable,
        fill,
        code: h.id.clone().into(),
        ip: if h.ip.is_empty() { "—".into() } else { h.ip.clone().into() },
        country: cc.clone().unwrap_or_else(|| "—".into()).into(),
        guests: format!("{} / {}", h.guests, h.max_guests).into(),
        access: if h.has_password { "по паролю".into() } else { "открытый".into() },
        proto: proto_name(&h.protocol).into(),
        proto_id: h.protocol.clone().into(),
        // Пусто до раскрытия карточки: мерить отклик для строк, на которые никто
        // не смотрит, — зря дёргать чужие машины.
        ping: ping_text,
        ping_ms,
    }
}

/// Фон: каждые 3с — статус координатора + каталог; наполняем UI.
#[allow(clippy::too_many_arguments)]
fn spawn_refresh(
    engine: EngineSlot,
    hosts: Arc<Mutex<Vec<HostInfo>>>,
    weak: Weak<AppWindow>,
    handle: &tokio::runtime::Handle,
    host_started: Ts,
    vpn_since: Ts,
    recent_ids: Arc<Mutex<Vec<String>>>,
    my_ip_slot: Arc<Mutex<String>>,
    up_file: std::path::PathBuf,
    pings: Pings,
) {
    let pings = pings.clone();
    // ── Тик 1с: часы сессии + примирение статуса по файл-маркеру хелпера.
    // Отдельно от каталога: watch может тихо ждать до 25с, а часы должны идти.
    {
        let (weak, vpn_since, host_started, up_file, hosts) =
            (weak.clone(), vpn_since.clone(), host_started.clone(), up_file.clone(), hosts.clone());
        handle.spawn(async move {
            loop {
                // Надёжный сигнал «туннель поднят» — файл-маркер от хелпера (id хоста).
                // На macOS TCP-STATE до окна мог не дойти; файл читаем стабильно.
                let helper_up: Option<String> = std::fs::read_to_string(&up_file).ok()
                    .map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
                // Файл — авторитет: часы сессии идут РОВНО пока туннель поднят.
                // Так на ВСЕХ платформах: Windows раньше маркер не писала, и
                // потерянный STATE было нечем догнать.
                {
                    let mut vs = vpn_since.lock().unwrap();
                    if helper_up.is_some() {
                        if vs.is_none() { *vs = Some(std::time::Instant::now()); }
                    } else {
                        *vs = None;
                    }
                }
                let vpn_el = vpn_since.lock().unwrap().map(|t| uptime_text(t.elapsed().as_secs()));
                let host_el = host_started.lock().unwrap().map(|t| uptime_text(t.elapsed().as_secs()));
                let (weak2, names) = (weak.clone(), hosts.clone());
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak2.upgrade() else { return };
                    // Туннель поднят, а UI застрял на «Подключаюсь…» → догоняем.
                    if let Some(id) = &helper_up {
                        if ui.get_vpn_state() == 1 {
                            ui.set_vpn_state(2);
                            ui.set_vpn_host_id(id.clone().into());
                            let name = {
                                let hs = names.lock().unwrap();
                                if let Some(h) = hs.iter().find(|h| &h.id == id) {
                                    fill_vpn_card(&ui, h); // догнали статус — догоняем и ячейки
                                    display_name(h)
                                } else {
                                    id.clone()
                                }
                            };
                            ui.set_vpn_status(name.into());
                            ui.set_vpn_sub("".into());
                        }
                    }
                    // …и НАОБОРОТ: маркера нет — значит туннеля нет. Маркер вводили
                    // потому, что STATE по TCP на «спящем» event-loop macOS до окна
                    // доходит не всегда; проверялось только его появление, поэтому
                    // потерянный STATE 0 оставлял карточку «подключено» навсегда
                    // (часы при этом уже стояли — vpn_since гасится симметрично).
                    // Маркер пишется ДО отправки STATE 2, так что «подключились, но
                    // файла ещё нет» не бывает — ложного сброса не будет.
                    if helper_up.is_none() && ui.get_vpn_state() == 2 {
                        ui.set_vpn_state(0);
                        ui.set_vpn_host_id("".into());
                        ui.set_vpn_status("VPN выключен".into());
                        ui.set_vpn_sub("".into());
                    }
                    if ui.get_vpn_state() == 2 { ui.set_vpn_elapsed(vpn_el.unwrap_or_default().into()); }
                    if ui.get_host_state() == 2 { ui.set_host_elapsed(host_el.unwrap_or_default().into()); }
                });
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });
    }

    // ── Живой каталог: watch отвечает МГНОВЕННО при изменении (гость зашёл/
    // вышел, хост появился) — счётчики и список без опозданий опроса.
    handle.spawn(async move {
        let mut my_ip = String::new();
        let mut hist_top = String::new();
        let mut ver = 0u64;
        loop {
            let eng = engine.lock().unwrap().clone();
            // Смена координатора пересоздаёт движок — прерываем ожидание watch.
            let upd = tokio::select! {
                r = eng.guest_watch(None, false, ver) => Some(r),
                _ = async {
                    loop {
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        if !Arc::ptr_eq(&eng, &engine.lock().unwrap()) { break; }
                    }
                } => None,
            };
            let Some(upd) = upd else { ver = 0; continue };
            // Каталог: обновляем ТОЛЬКО при успехе; на Err старый список НЕ трём.
            let list = match upd {
                Ok(u) => { ver = u.version; Some(u.hosts) }
                Err(_) => { ver = 0; None }
            };
            // СТАТУС связи = состояние WS-сокета (health), а НЕ успех watch: сокет
            // жив, пока снапшот каталога ещё в пути (хост уже анонсирован).
            let t0 = std::time::Instant::now();
            let alive = eng.coordinator_health().await.is_ok();

            let ping = t0.elapsed().as_millis() as i32;
            if alive && my_ip.is_empty() {
                if let Ok(ip) = eng.my_ip().await {
                    my_ip = ip.clone();
                    *my_ip_slot.lock().unwrap() = ip;
                }
            }
            // История серверов — добавляем рабочий адрес (как iOS addServerHistory).
            let hist = if alive {
                let cur = eng.config().coordinators.first().cloned().unwrap_or_default();
                if cur != hist_top { hist_top = cur.clone(); Some(store::add_server_history(&cur)) } else { None }
            } else { None };

            let hosts_n = list.as_ref().map(|l| l.len() as i32);
            let ip = my_ip.clone();
            let recent = recent_ids.lock().unwrap().clone();
            let weak = weak.clone();
            let hosts2 = hosts.clone();
            let pings = pings.clone();   // клон на КАЖДУЮ итерацию: замыкание его забирает
            let _ = slint::invoke_from_event_loop(move || {
                let Some(ui) = weak.upgrade() else { return };
                ui.set_coord_state(if alive { 1 } else { 2 });

                ui.set_coord_ping(if alive { ping } else { 0 });
                ui.set_coord_ip(ip.into());
                if let Some(h) = hist {
                    ui.set_server_history(str_model(&h.iter().map(|u| pretty_url(u)).collect::<Vec<_>>()));
                }
                if let Some(n) = hosts_n { ui.set_coord_hosts(n); }

                // Каталог обновляем ТОЛЬКО когда пришёл (watch Ok); на Err оставляем
                // прошлый список — не мигаем «пусто» на живом соединении.
                let Some(list) = list else { return };
                // Видимость (как displayedHosts на iOS): онлайн + подключённый +
                // свой раздающийся; прячем свой «призрак» после стопа раздачи.
                let connected_to = ui.get_vpn_host_id().to_string();
                let hosting = ui.get_host_state() == 2 || ui.get_host_state() == 1;
                let my_code = ui.get_host_code().to_string();
                let visible: Vec<HostInfo> = list.iter()
                    .filter(|h| (h.online || h.id == connected_to || (hosting && h.id == my_code))
                        && !(!hosting && !my_code.is_empty() && h.id == my_code))
                    .cloned().collect();
                let rows: Vec<HostRow> = visible.iter().map(|h| host_row(h, &pings)).collect();
                *hosts2.lock().unwrap() = visible;
                ui.set_hosts(ModelRc::new(VecModel::from(rows)));

                // Недавние — только те, что сейчас ОНЛАЙН в каталоге (как iOS).
                let mut r_names = Vec::new();
                let mut r_ids = Vec::new();
                for id in &recent {
                    if let Some(h) = list.iter().find(|h| &h.id == id && h.online) {
                        r_names.push(display_name(h));
                        r_ids.push(id.clone());
                    }
                }
                ui.set_recent_names(str_model(&r_names));
                ui.set_recent_ids(str_model(&r_ids));

                // Инфо-блок своей раздачи (часы ведёт тик-задача).
                if ui.get_host_state() == 2 {
                    let id = ui.get_host_code().to_string();
                    if let Some(h) = list.iter().find(|h| h.id == id) {
                        ui.set_host_ip(if h.ip.is_empty() { "—".into() } else { h.ip.clone().into() });
                        ui.set_host_guests(format!("{} / {}", h.guests, h.max_guests).into());
                    }
                }
                // Инфо-блок подключения (часы ведёт тик-задача). Здесь только
                // ОБНОВЛЕНИЕ живых цифр: первично карточку заполняет fill_vpn_card
                // в момент подключения — иначе поля пустуют до следующего ответа
                // каталога (длинный опрос, до ~25с), а для скрытого хоста навсегда.
                if ui.get_vpn_state() == 2 {
                    let id = ui.get_vpn_host_id().to_string();
                    if let Some(h) = list.iter().find(|h| h.id == id) {
                        fill_vpn_card(&ui, h);
                    }
                }
            });
            if !alive {
                tokio::time::sleep(Duration::from_secs(2)).await; // не молотить при обрыве
            }
        }
    });
}

// ── VPN (гость): команды root-хелперу, статусы приходят в on_state ──────────

fn wire_vpn(
    ui: &AppWindow,
    hosts: Arc<Mutex<Vec<HostInfo>>>,
    hlp: Rc<helper::Helper>,
    my_ip: Arc<Mutex<String>>,
    coord_full: Arc<Mutex<String>>,
) {
    // Оптимистичный статус до ответа хелпера (как iOS connect()).
    fn begin(ui: &AppWindow, id: &str, name: &str) {
        ui.set_vpn_state(1);
        ui.set_vpn_status("Подключаюсь…".into());
        ui.set_vpn_sub(format!("к {name}").into());
        ui.set_vpn_host_id(id.into());
        // Чистим карточку: иначе от прошлого подключения остаются чужие IP и
        // счётчик гостей, пока не придёт каталог. Дальше заполнит fill_vpn_card.
        ui.set_vpn_ip("—".into());
        ui.set_vpn_country("—".into());
        ui.set_vpn_guests("—".into());
        ui.set_vpn_proto("".into());
        ui.set_vpn_proto_id("".into());
    }
    fn busy(ui: &AppWindow) -> bool {
        let s = ui.get_vpn_state();
        s == 1 || s == 2
    }

    // Подключение по индексу карточки.
    {
        let weak = ui.as_weak();
        let (hosts, hlp) = (hosts.clone(), hlp.clone());
        let coord_full = coord_full.clone();
        ui.on_connect(move |idx| {
            let Some(u) = weak.upgrade() else { return };
            if busy(&u) { return; }
            let Some(host) = hosts.lock().unwrap().get(idx as usize).cloned() else { return };
            if u.get_host_state() == 2 && u.get_host_code() == host.id.as_str() {
                u.set_vpn_state(3);
                u.set_vpn_status("Это ваш собственный хост".into());
                return;
            }
            let pw = if host.has_password { u.get_guest_password().to_string() } else { String::new() };
            u.set_guest_password("".into());
            begin(&u, &host.id, &display_name(&host));
            fill_vpn_card(&u, &host); // данные уже на руках — не ждём ответа каталога
            hlp.connect(&coord_full.lock().unwrap(), &host.id, &pw, &host.protocol);
        });
    }

    // Подключение по коду.
    {
        let weak = ui.as_weak();
        let (hosts, hlp) = (hosts.clone(), hlp.clone());
        let coord_full = coord_full.clone();
        ui.on_connect_code(move || {
            let Some(u) = weak.upgrade() else { return };
            if busy(&u) { return; }
            let code = u.get_code().to_string().trim().to_uppercase();
            if code.is_empty() { return; }
            if u.get_host_state() == 2 && u.get_host_code() == code.as_str() {
                u.set_vpn_state(3);
                u.set_vpn_status("Это код вашего же хоста".into());
                u.set_code("".into());
                return;
            }
            u.set_code("".into());
            begin(&u, &code, &code);
            // Код мог быть и от хоста из каталога — тогда цифры есть сразу.
            if let Some(h) = hosts.lock().unwrap().iter().find(|h| h.id == code) {
                fill_vpn_card(&u, h);
            }
            hlp.connect(&coord_full.lock().unwrap(), &code, "", "");
        });
    }

    // Вставить код из буфера.
    {
        let weak = ui.as_weak();
        ui.on_paste(move || {
            if let Ok(mut cb) = arboard::Clipboard::new() {
                if let Ok(txt) = cb.get_text() {
                    let clean: String = txt.chars().filter(|c| c.is_ascii_alphanumeric()).take(16).collect::<String>().to_uppercase();
                    if let Some(ui) = weak.upgrade() { ui.set_code(clean.into()); }
                }
            }
        });
    }

    // Умный «Старт»: упорядоченная очередь кандидатов, перебирает хелпер
    // (чужая страна раньше, затем больше свободных мест — как iOS).
    {
        let weak = ui.as_weak();
        let (hosts, hlp, my_ip) = (hosts, hlp.clone(), my_ip);
        let coord_full = coord_full.clone();
        ui.on_quick_connect(move || {
            let Some(u) = weak.upgrade() else { return };
            if busy(&u) { return; }
            let own = if u.get_host_state() == 2 { u.get_host_code().to_string() } else { String::new() };
            let mine = geo::country_of(&my_ip.lock().unwrap());
            let mut cands: Vec<HostInfo> = hosts.lock().unwrap().iter()
                .filter(|h| h.online && !h.has_password && h.guests < h.max_guests && h.id != own)
                .cloned().collect();
            cands.sort_by(|a, b| {
                if let Some(m) = &mine {
                    let af = host_cc(a).as_ref() != Some(m);
                    let bf = host_cc(b).as_ref() != Some(m);
                    if af != bf { return bf.cmp(&af); }
                }
                (b.max_guests - b.guests).cmp(&(a.max_guests - a.guests))
            });
            let Some(first) = cands.first() else {
                u.set_vpn_state(3);
                u.set_vpn_status("Нет открытого свободного хоста".into());
                return;
            };
            begin(&u, &first.id, &display_name(first));
            // Берём только несколько лучших по сортировке выше. Раньше в хелпер
            // уходил ВЕСЬ каталог: при сотне свободных хостов перебор шёл бы
            // минутами, а человек всё это время смотрел бы на «подключаюсь».
            // Если не подошёл никто из первой пятёрки — проблема не в хостах.
            const QUICK_MAX: usize = 5;
            let list: Vec<(String, String)> = cands.iter().take(QUICK_MAX)
                .map(|h| (h.id.clone(), h.protocol.clone())).collect();
            hlp.quick(&coord_full.lock().unwrap(), &list);
        });
    }

    // Отключение: хелпер шлёт BYE и откатывает маршруты; UI гасим сразу.
    {
        let weak = ui.as_weak();
        ui.on_disconnect(move || {
            hlp.stop();
            if let Some(u) = weak.upgrade() {
                u.set_vpn_state(0);
                u.set_vpn_host_id("".into());
                u.set_vpn_status("VPN выключен".into());
                u.set_vpn_sub("".into());
            }
        });
    }
}

// ── Хост (раздача) ───────────────────────────────────────────────────────────

fn wire_host(
    ui: &AppWindow,
    engine: EngineSlot,
    host_task: Rc<RefCell<Option<tokio::task::JoinHandle<()>>>>,
    handle: tokio::runtime::Handle,
    host_started: Ts,
    // Свой внешний IP — из него берётся страна для имени хоста по умолчанию.
    my_ip: Arc<Mutex<String>>,
) {
    let host_engine: Arc<Mutex<Option<Arc<BmvEngine>>>> = Arc::new(Mutex::new(None));
    let engine_nc = engine.clone(); // для «Новый код» (engine уходит в toggle-замыкание)

    let handle_nc = handle.clone();
    let hs_nc = host_started.clone();
    let weak = ui.as_weak();
    let ht = host_task.clone();
    let heng = host_engine.clone();
    let byhandle = handle.clone();
    let hs = host_started.clone();
    ui.on_toggle_host(move || {
        let state = weak.upgrade().map(|u| u.get_host_state()).unwrap_or(0);
        if state == 1 || state == 2 {
            if let Some(eng) = heng.lock().unwrap().take() {
                byhandle.spawn(async move { let _ = tokio::time::timeout(Duration::from_secs(3), eng.host_deannounce()).await; });
            }
            if let Some(h) = ht.borrow_mut().take() { h.abort(); }
            *hs.lock().unwrap() = None;
            set_host(&weak, 0, "Раздача выключена".into(), String::new());
            return;
        }
        let (name, public, max, password, protocol) = weak.upgrade()
            .map(|u| (u.get_host_name().to_string(), u.get_host_public(), u.get_host_max().max(1) as u32,
                      u.get_host_password().to_string(), u.get_host_protocol().to_string()))
            .unwrap_or_else(|| (String::new(), true, 8, String::new(), "noise".into()));
        let base = engine.lock().unwrap().config().clone();
        let (weak, heng2, hs2) = (weak.clone(), heng.clone(), hs.clone());
        if let Some(ui) = weak.upgrade() {
            ui.set_host_state(1);
            ui.set_host_status("Запускаюсь…".into());
            ui.set_host_error("".into());
            ui.set_host_code("".into());
        }
        let task = handle.spawn(async move {
            let build = |id: &str, sig: &str| {
                let mut cfg = base.clone();
                cfg.host.name = name.clone();
                cfg.host.public = public;
                cfg.host.max_guests = max;
                cfg.host.password = password.clone();
                cfg.host.id = id.to_string();
                cfg.host.code_sig = sig.to_string();
                cfg.host.token = sig.to_string(); // стабильный owner-token = подпись (анти-угон)
                if !protocol.is_empty() { cfg.default_protocol = protocol.clone(); }
                cfg
            };
            let (mut id, mut sig) = store::load_host_creds();
            if id.is_empty() || sig.is_empty() {
                match BmvEngine::from_config(base.clone()).host_new_code().await {
                    Ok((c, s)) if !c.is_empty() && !s.is_empty() => { id = c; sig = s; store::save_host_creds(&id, &sig); }
                    _ => return set_host_err(&weak, "Сервер не выдал код".into()),
                }
            }
            let mut eng = Arc::new(BmvEngine::from_config(build(&id, &sig)));
            let mut announce = eng.host_bind_announce().await;
            if announce.as_ref().err().map(|e| e.to_string().contains("403")).unwrap_or(false) {
                store::clear_host_creds();
                if let Ok((c, s)) = BmvEngine::from_config(base.clone()).host_new_code().await {
                    if !c.is_empty() && !s.is_empty() {
                        id = c; sig = s; store::save_host_creds(&id, &sig);
                        eng = Arc::new(BmvEngine::from_config(build(&id, &sig)));
                        announce = eng.host_bind_announce().await;
                    }
                }
            }
            let hub = match announce {
                Ok((hub, _id, _eps)) => hub,
                Err(e) => {
                    let s = e.to_string();
                    let msg = if s.contains("422") { "Нет публичного адреса (вы за NAT) — раздача отсюда невозможна".to_string() } else { s };
                    return set_host_err(&weak, msg);
                }
            };
            *heng2.lock().unwrap() = Some(eng.clone());
            *hs2.lock().unwrap() = Some(std::time::Instant::now());
            set_host(&weak, 2, "Раздаю интернет".into(), eng.host_id().to_string());

            let mut set = tokio::task::JoinSet::new();
            { let (e, h) = (eng.clone(), hub.clone()); set.spawn(async move { loop { tokio::time::sleep(Duration::from_secs(10)).await; let _ = e.host_heartbeat(&h).await; } }); }
            { let (e, h) = (eng.clone(), hub.clone()); set.spawn(async move { let _ = e.host_serve_punch(h).await; }); }
            { let (e, h) = (eng.clone(), hub.clone()); set.spawn(async move {
                while let Some((peer, raw)) = h.accept().await {
                    let e = e.clone();
                    tokio::spawn(async move { let _ = e.host_run_session(peer, raw, true).await; });
                }
            }); }
            while set.join_next().await.is_some() {}
            heng2.lock().unwrap().take();
            *hs2.lock().unwrap() = None;
            set_host(&weak, 0, "Раздача выключена".into(), String::new());
        });
        *ht.borrow_mut() = Some(task);
    });

    // Копировать код сети.
    {
        let weak = ui.as_weak();
        ui.on_copy_code(move || {
            let code = weak.upgrade().map(|u| u.get_host_code().to_string()).unwrap_or_default();
            if !code.is_empty() {
                if let Ok(mut cb) = arboard::Clipboard::new() { let _ = cb.set_text(code); }
            }
        });
    }

    // «Новый код»: сброс + bye + авто-рестарт под свежим (как iOS newHostCode).
    {
        let weak = ui.as_weak();
        let (heng2, ht2, byh, hs_nc) = (host_engine.clone(), host_task.clone(), handle_nc.clone(), hs_nc);
        ui.on_new_code(move || {
            store::clear_host_creds();
            let was = weak.upgrade().map(|u| u.get_host_state() == 2 || u.get_host_state() == 1).unwrap_or(false);
            if let Some(eng) = heng2.lock().unwrap().take() {
                byh.spawn(async move { let _ = tokio::time::timeout(Duration::from_secs(3), eng.host_deannounce()).await; });
            }
            if let Some(h) = ht2.borrow_mut().take() { h.abort(); }
            *hs_nc.lock().unwrap() = None;
            set_host(&weak, 0, "Раздача выключена".into(), String::new());
            if was {
                // Раздавали → авто-рестарт: toggle сам запросит свежий код.
                let weak2 = weak.clone();
                byh.spawn(async move {
                    tokio::time::sleep(Duration::from_millis(800)).await;
                    let _ = slint::invoke_from_event_loop(move || { if let Some(ui) = weak2.upgrade() { ui.invoke_toggle_host(); } });
                });
            } else {
                // Не раздавали → просто получить и показать свежий код (как iOS).
                let eng = engine_nc.lock().unwrap().clone();
                let weak2 = weak.clone();
                byh.spawn(async move {
                    if let Ok((code, s)) = eng.host_new_code().await {
                        if !code.is_empty() && !s.is_empty() {
                            store::save_host_creds(&code, &s);
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(ui) = weak2.upgrade() { ui.set_host_code(code.into()); }
                            });
                        }
                    }
                });
            }
        });
    }

    // «Применить изменения» на лету.
    {
        let weak = ui.as_weak();
        let (heng2, hap) = (host_engine, handle_nc);
        let my_ip2 = my_ip;
        ui.on_apply_host(move || {
            let Some(eng) = heng2.lock().unwrap().clone() else { return };
            let Some(u) = weak.upgrade() else { return };
            let (name, public, max, password, protocol) = (
                u.get_host_name().to_string(), u.get_host_public(), u.get_host_max().max(1) as u32,
                u.get_host_password().to_string(), u.get_host_protocol().to_string());
            // Имя по умолчанию считаем ДО задачи: гард мьютекса через await не живёт.
            let fallback_name = default_host_name(&my_ip2.lock().unwrap().clone());
            hap.spawn(async move {
                let name = if name.trim().is_empty() { fallback_name } else { name };
                let _ = eng.host_set_name(&name).await;
                let _ = eng.host_set_max_guests(max).await;
                let _ = eng.host_set_password(&password).await;
                let _ = eng.host_set_protocol(&protocol).await;
                let _ = eng.host_set_public(public).await;
            });
        });
    }
}

fn set_host(weak: &Weak<AppWindow>, state: i32, text: String, code: String) {
    let weak = weak.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_host_state(state);
            ui.set_host_status(text.into());
            // Пустой код = сохранить прежний (после стопа код всё равно виден, как
            // на iOS/Android). Реально меняем только на непустой (старт/новый код).
            if !code.is_empty() { ui.set_host_code(code.into()); }
            if state != 3 { ui.set_host_error("".into()); }
        }
    });
}

fn set_host_err(weak: &Weak<AppWindow>, msg: String) {
    let weak = weak.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_host_state(0);
            ui.set_host_status("Раздача выключена".into());
            // Код НЕ сбрасываем — он валиден, пригодится для повторной попытки.
            ui.set_host_error(msg.into());
        }
    });
}

// ── Сервер (смена координатора) ──────────────────────────────────────────────

/// Адрес для ПОКАЗА — без схемы.
///
/// Соединение у нас всегда https, поэтому «https://» перед каждым адресом
/// занимает место и ничего не сообщает. Хранение и запросы схему сохраняют —
/// срезается она только на экране.
pub fn pretty_url(url: &str) -> String {
    url.trim_start_matches("https://").trim_start_matches("http://").trim_end_matches('/').to_string()
}

/// Обратно: то, что ввёл человек, → пригодный адрес.
///
/// Раз схему мы не показываем, набирать её человек тоже не станет — «bemyvpn.net»
/// обязано работать. Голому имени приписываем https: небезопасную схему нужно
/// указать явно, случайно на неё не свалишься.
fn full_url(input: &str) -> String {
    let t = input.trim().trim_end_matches('/');
    if t.starts_with("http://") || t.starts_with("https://") { t.to_string() } else { format!("https://{t}") }
}

fn wire_coord(ui: &AppWindow, engine: EngineSlot, coord_full: Arc<Mutex<String>>) {
    let apply = {
        let engine = engine.clone();
        let weak = ui.as_weak();
        move |url: String| {
            let url = full_url(&url);
            if url.len() <= "https://".len() { return; }
            *coord_full.lock().unwrap() = url.clone();
            let mut cfg = engine.lock().unwrap().config().clone();
            cfg.coordinators = vec![url.clone()];
            *engine.lock().unwrap() = Arc::new(BmvEngine::from_config(cfg));
            if let Some(ui) = weak.upgrade() {
                ui.set_coord_is_default(url == DEFAULT_COORD);
                ui.set_coord_url(pretty_url(&url).into());
                ui.set_coord_field(pretty_url(&url).into());
                ui.set_coord_state(0);
            }
        }
    };
    let apply_set = apply.clone();
    ui.on_set_coord(move |url| apply_set(url.to_string()));
    let apply_reset = apply.clone();
    ui.on_reset_coord(move || apply_reset(DEFAULT_COORD.to_string()));
}

// ── QR приглашения ───────────────────────────────────────────────────────────

fn qr_image(text: &str) -> Option<slint::Image> {
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
    let colors = code.to_colors();
    let w = code.width();
    // Поле — 2 модуля, а не 4: белая карточка под картинкой даёт свой отступ,
    // и вместе с запечённым получалась двойная рамка (на Android поле вообще 0,
    // роль тихой зоны играет карточка). Меньше поле — крупнее сам код.
    let (quiet, scale) = (2usize, 8usize);
    let dim = (w + quiet * 2) * scale;
    let mut buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(dim as u32, dim as u32);
    let px = buf.make_mut_slice();
    for p in px.iter_mut() { *p = slint::Rgba8Pixel { r: 255, g: 255, b: 255, a: 255 }; }
    for y in 0..w {
        for x in 0..w {
            if colors[y * w + x] == qrcode::Color::Dark {
                for dy in 0..scale {
                    for dx in 0..scale {
                        let py = (y + quiet) * scale + dy;
                        let pxx = (x + quiet) * scale + dx;
                        px[py * dim + pxx] = slint::Rgba8Pixel { r: 16, g: 16, b: 22, a: 255 };
                    }
                }
            }
        }
    }
    Some(slint::Image::from_rgba8(buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Строка каталога собирается, когда замер отклика УЖЕ ЕСТЬ.
    ///
    /// Ловит самодедлок: раньше `host_row` брала `pings.lock()` дважды в одном
    /// литерале структуры. Временные значения живут до конца оператора, поэтому
    /// первый guard был жив при взятии второго, а `std::sync::Mutex` не
    /// реентрантный — поток вставал навсегда. И вставал он на потоке событий:
    /// окно замирало целиком.
    ///
    /// Пустой каталог этого НЕ ловил — строк нет, вызова нет. Поэтому тест
    /// обязан класть значение в карту и обязан ограничивать время: дедлок не
    /// падает, он висит.
    /// Схема срезается для показа и возвращается при вводе.
    ///
    /// Раз «https://» на экране не показывается, набирать его человек тоже не
    /// станет — «bemyvpn.net» обязано работать. А голому имени нельзя молча
    /// приписать http: незашифрованный координатор нужно выбирать осознанно.
    #[test]
    fn address_loses_its_scheme_on_screen_but_keeps_it_in_use() {
        assert_eq!(pretty_url("https://bemyvpn.net"), "bemyvpn.net");
        assert_eq!(pretty_url("http://10.0.0.5:3330"), "10.0.0.5:3330");
        assert_eq!(pretty_url("https://bemyvpn.net/"), "bemyvpn.net");

        assert_eq!(full_url("bemyvpn.net"), "https://bemyvpn.net");
        assert_eq!(full_url("  bemyvpn.net/ "), "https://bemyvpn.net");
        // Явную схему не трогаем — в том числе незашифрованную.
        assert_eq!(full_url("http://10.0.0.5:3330"), "http://10.0.0.5:3330");

        // Показали → ввели обратно → адрес тот же.
        for u in ["https://bemyvpn.net", "http://10.0.0.5:3330"] {
            let shown = pretty_url(u);
            let back = full_url(&shown);
            assert_eq!(back.trim_start_matches("https://").trim_start_matches("http://"), shown);
        }
    }

    /// Показ и работа используют РАЗНЫЕ формы адреса — и путать их нельзя.
    ///
    /// На экране адрес идёт доменом. Но тем же значением подключаются и по нему
    /// же ключуются «Недавние». Подставь туда домен — запрос не соберётся, а
    /// недавние запишутся под один ключ и прочитаются под другой, то есть тихо
    /// исчезнут.
    #[test]
    fn display_address_is_never_the_one_used_for_work() {
        let full = "https://bemyvpn.net";
        let shown = pretty_url(full);
        assert_ne!(shown, full, "показ и работа обязаны различаться");
        // Полный адрес разбирается как URL, домен — нет.
        assert!(full.starts_with("https://"));
        assert!(!shown.contains("://"));
        // Из показанного всегда восстанавливается рабочий.
        assert_eq!(full_url(&shown), full);
    }

    #[test]
    fn building_a_row_with_a_known_ping_does_not_deadlock() {
        let pings: Pings = Arc::new(Mutex::new(std::collections::HashMap::new()));
        pings.lock().unwrap().insert("h1".into(), Some(42));
        pings.lock().unwrap().insert("h2".into(), None);

        // `HostRow` содержит slint::Image и потому не Send — через канал едут
        // уже снятые с неё значения.
        let (tx, rx) = std::sync::mpsc::channel();
        let p = pings.clone();
        std::thread::spawn(move || {
            let seen: Vec<(String, i32)> = ["h1", "h2", "h3"]
                .iter()
                .map(|id| {
                    let h = HostInfo { id: (*id).into(), ..HostInfo::default() };
                    let r = host_row(&h, &p);
                    (r.ping.to_string(), r.ping_ms)
                })
                .collect();
            let _ = tx.send(seen);
        });

        let seen = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("host_row зависла — снова двойной lock в одном выражении");

        assert_eq!(seen[0], ("42 мс".to_string(), 42));
        assert_eq!(seen[1], ("—".to_string(), -1));
        assert_eq!(seen[2], (String::new(), -1));
    }
}
