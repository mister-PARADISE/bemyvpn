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
///
/// ПУСТАЯ СТРОКА — ЭТО «ОБЫЧНЫЙ», А НЕ «БЕЗ ШИФРА». Хост, не объявивший
/// протокол в каталоге, всё равно поднимает шифрованный канал (noise — значение
/// по умолчанию у ядра). Раньше такой хост показывался как незашифрованный:
/// человек видел «Без шифра» там, где шифр есть, и та же самая запись каталога
/// в терминале подписывалась «Обычный» — две оболочки говорили противоположное
/// об одном и том же хосте.
///
/// Незнакомое имя показываем КАК ЕСТЬ. Врать «Без шифра» про неизвестный
/// протокол так же неверно, как врать про пустой: мы про него ничего не знаем.
fn proto_name(p: &str) -> &str {
    match p {
        "" | "noise" | "noise-aes" => "Обычный",
        "noise-obfs" => "Маскировка",
        "plain" => "Без шифра",
        other => other,
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

/// Скачать релиз и запустить установку.
///
/// Целостность обеспечивает ТОЛЬКО HTTPS до github.com. Ни sha256, ни подписи
/// здесь не проверяется — раньше об этом врал и заголовок, и комментарий у
/// вызова кнопки «Обновить». Отдельная проверка хэша, скачанного по тому же
/// каналу, ничего бы и не добавила: подменивший файл подменит и хэш.
///
/// Возврат Ok означает «помощник запущен» — приложение после этого обязано
/// завершиться, иначе подмена не пройдёт: пока процесс жив, файлы держатся.
async fn fetch_update(tag: &str) -> Result<(), String> {
    let asset = bmv_common::update::current_asset_name(true)
        .ok_or("Для вашей системы обновления не выпускаются.")?;
    let repo = std::env::var("BMV_REPO").unwrap_or_else(|_| "mister-PARADISE/bemyvpn".into());
    let url = bmv_common::update::asset_url(&repo, tag, asset);

    let bytes = bmv_common::update::download(&url, bmv_common::update::MAX_ASSET_BYTES)
        .await
        // Частый случай: GitHub заблокирован. Наш же продукт это и решает.
        .map_err(|_| "Не удалось скачать обновление — подключитесь к VPN и повторите.".to_string())?;

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

/// Проектная ширина макета. Все размеры внутри (шрифты, значки, отступы) заданы
/// в точках жёстко и от ширины окна НЕ зависят — значит и ширина обязана быть
/// ровно та, под которую верстали.
const LAYOUT_W: f32 = 400.0;

/// Габариты окна: ширина ВСЕГДА проектная, от экрана берётся только высота.
///
/// Раньше ширина ВЫВОДИЛАСЬ из высоты по пропорции 400:820 — «чтобы держалась
/// пропорция». Держалась она, а макет — нет: на экране в 800 точек высоты
/// выходило окно шириной 273 точки вместо 400, и то же самое содержимое с теми
/// же кеглями и значками лезло в две трети ширины. Читалось это как
/// «приближено»: буквы и значки крупные, строки зажаты. Пропорция макету не
/// нужна — нужна его ширина.
///
/// Высоту считаем от экрана: прибивать 820 намертво нельзя — на ноутбуке с
/// экраном в 800 точек окно не помещалось и уезжало за нижний край. По высоте
/// всё содержимое прокручивается, так что лишняя высота только на пользу —
/// видно больше списка.
fn window_size() -> (f32, f32) {
    let screen_h = display_info::DisplayInfo::all()
        .ok()
        .and_then(|list| {
            list.iter()
                .find(|d| d.is_primary)
                .or(list.first())
                .map(|d| logical_screen_h(d.height as f32, d.scale_factor, SCREEN_H_IS_PHYSICAL))
        })
        .unwrap_or(1080.0);

    (LAYOUT_W, fit_height(screen_h))
}

/// Отдаёт ли `display-info` высоту экрана в ФИЗИЧЕСКИХ пикселях (а не в точках).
///
/// Крейт единиц не документирует, и они у него РАЗНЫЕ по платформам — проверено
/// по его исходникам (display-info 0.5.9):
///
/// * macOS (`src/macos/mod.rs`): высота = `CGDisplayBounds(id).size.height`, то
///   есть ТОЧКИ; сам `scale_factor` там и считается как
///   `pixel_width / bounds.width`. Retina 2214×2 отдаёт 1107 — делить нельзя.
/// * Linux/xorg (`src/linux/xorg.rs`): крейт САМ делит на `scale_factor` —
///   тоже точки.
/// * Windows (`src/windows/mod.rs`): высота = `dmPelsHeight` из
///   `EnumDisplaySettingsW`, а «Pels» — это пиксели РЕЖИМА монитора, масштаб
///   Windows в них не заложен вообще.
///
/// Отсюда и брался слишком высокий экран на Windows: FullHD при масштабе 150%
/// даёт `height = 1080`, `scale_factor = 1.5` (наш манифест объявляет
/// PerMonitorV2, поэтому крейт берёт настоящий `GetDpiForMonitor`), а Slint
/// рисует в 1080/1.5 = 720 точках. Мы просили 0.75·1080 = 770 точек контента —
/// то есть окно ВЫШЕ всего экрана, а не три четверти от него.
const SCREEN_H_IS_PHYSICAL: bool = cfg!(windows);

/// Высота экрана в ЛОГИЧЕСКИХ точках — тех же, в которых считает Slint.
///
/// Флаг `physical` передаётся аргументом, а не читается из `cfg!` внутри, чтобы
/// тест мог проверить ОБЕ ветки на любой машине: разницу единиц между macOS и
/// Windows иначе видно только на самой Windows.
fn logical_screen_h(height: f32, scale_factor: f32, physical: bool) -> f32 {
    // Масштаб приходит из чужого кода. Ноль или NaN дали бы бесконечную «высоту
    // экрана», а из неё — окно размером с бесконечность; лучше считать, что
    // масштаба нет.
    if physical && scale_factor.is_finite() && scale_factor > 0.0 {
        height / scale_factor
    } else {
        height
    }
}

/// Высота содержимого окна по высоте экрана.
///
/// Вынесена отдельно, чтобы проверять тестом на разных экранах: на живом окне
/// видно только свой.
fn fit_height(screen_h: f32) -> f32 {
    /// Какую часть экрана занимает окно ЦЕЛИКОМ, вместе с заголовком.
    ///
    /// Оставшаяся пятая часть — системные панели (строка меню и док на macOS,
    /// панель задач на Windows) и просто воздух: окно впритык к краю читается
    /// как «не поместилось», даже когда поместилось. Было 0.75 — окно просили
    /// сделать чуть больше, и запаса в 20% на панели хватает.
    const SHARE: f32 = 0.80;
    /// Заголовок рисует ОС поверх запрошенной высоты.
    const TITLE_BAR: f32 = 40.0;
    /// Ниже список хостов вырождается в щель — лучше нарушить долю.
    const MIN_H: f32 = 520.0;

    (screen_h * SHARE - TITLE_BAR).max(MIN_H).round()
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

    // Битый конфиг НЕ ЗАМАЛЧИВАЕМ. Раньше стояло `unwrap_or_default()`: одна
    // опечатка в .toml — и приложение молча брало настройки по умолчанию, то
    // есть уезжало на СТАНДАРТНЫЙ координатор, хотя человек прописал свой. Со
    // стороны это выглядит как «сохранённый сервер сам сбросился».
    let (config, config_error) = match Config::load(None) {
        Ok(c) => (c, String::new()),
        Err(e) => (Config::default(), format!("Настройки не прочитались, взяты стандартные: {e}")),
    };
    let coord = config.coordinators.first().cloned().unwrap_or_else(|| DEFAULT_COORD.into());
    // Настройки хоста показываем ровно те, что сохранены (иначе имя, лимит,
    // пароль, протокол и видимость сбрасывались при каждом запуске).
    let saved_host = config.host.clone();
    let saved_proto = config.default_protocol.clone();
    let engine: EngineSlot = Arc::new(Mutex::new(Arc::new(BmvEngine::from_config(config))));

    let ui = AppWindow::new()?;
    {
        let (w, h) = window_size();
        ui.set_win_w(w);
        ui.set_win_h(h);
        // Тянуть окно мышью разрешено только на Windows — см. `resizable` в
        // app.slint.
        ui.set_resizable(cfg!(windows));
    }
    ui.set_coord_url(pretty_url(&coord).into());
    ui.set_coord_field(pretty_url(&coord).into());
    ui.set_coord_is_default(coord == DEFAULT_COORD);
    ui.set_config_error(config_error.into());
    ui.set_vpn_status("VPN выключен".into());
    ui.set_host_status("Раздача выключена".into());
    // Сохранённые настройки раздачи — на экран.
    ui.set_host_name(saved_host.name.into());
    ui.set_host_public(saved_host.public);
    ui.set_host_max(saved_host.max_guests.clamp(1, i32::MAX as u32) as i32);
    ui.set_host_password(saved_host.password.into());
    if !saved_proto.is_empty() {
        ui.set_host_protocol(saved_proto.into());
    }
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
    // TCP-STATE, который на idle event-loop macOS до окна мог не дойти).
    //
    // Лежит в СВОЁМ каталоге 0700, а не прямо в общем /tmp с именем по pid.
    // Писал его root, а на Linux /tmp общий: сосед по машине заранее клал по
    // предсказуемому пути ссылку на любой root-овый файл — и root затирал его.
    // Тем же файлом сосед рисовал в чужом окне ложное «подключено»: тик-цикл
    // ниже считает этот файл авторитетом состояния VPN.
    let up_dir = helper::private_dir();
    let up_file = up_dir.join("up");
    let _ = std::fs::remove_file(&up_file);

    // Копирование в буфер (тап по плиткам Код/IP).
    ui.on_copy_text(|t| {
        if !t.is_empty() {
            if let Ok(mut cb) = arboard::Clipboard::new() {
                let _ = cb.set_text(t.to_string());
            }
        }
    });

    // «Обновить»: скачать и установить. Единственная защита файла — HTTPS до
    // github.com (ни sha256, ни подписи мы не проверяем, см. fetch_update). На
    // десктопе установка идёт через помощника: приложение не может подменить
    // себя на ходу — оно выходит, помощник доделывает.
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
                // Запись хоста СНИМАЕТСЯ В ЛОКАЛЬНУЮ до всякого ветвления.
                // Раньше ниже стояло `if let Some(h) = hosts.lock()…find(…)`, и
                // guard мьютекса жил через ВСЁ тело ветки (edition 2021):
                // достаточно было тронуть `hosts` внутри — и поток интерфейса
                // вставал намертво. Дважды такое окно уже вешали.
                let found = hosts.lock().unwrap().iter().find(|h| h.id == id).cloned();
                let name = found.as_ref().map(display_name).unwrap_or_else(|| id.clone());
                match n {
                    1 => { ui.set_vpn_state(1); ui.set_vpn_host_id(id.clone().into()); ui.set_vpn_status("Подключаюсь…".into()); ui.set_vpn_sub(format!("к {name}").into()); }
                    2 => {
                        ui.set_vpn_state(2); ui.set_vpn_host_id(id.clone().into()); ui.set_vpn_status(name.into());
                        // Кто именно подключился, знает хелпер: при «Старте» он сам
                        // выбирает хост из списка кандидатов, и до этого момента id
                        // нам неизвестен. Заполняем карточку здесь — иначе ячейки
                        // пустуют до ответа каталога.
                        if let Some(h) = &found {
                            fill_vpn_card(&ui, h);
                        }
                        { let mut r = recent.lock().unwrap(); r.retain(|x| x != &id); r.insert(0, id.clone()); r.truncate(6); }
                        let _ = store::add_recent(&coord_full.lock().unwrap(), &id);
                    }
                    // ЗАГОЛОВОК КОРОТКИЙ И ПОСТОЯННЫЙ, объяснение — строкой ниже.
                    // Раньше текст ошибки уходил И в заголовок, И в подпись: одно и
                    // то же читалось дважды, а длинное сообщение (например, отказ
                    // из-за IPv6) разворачивалось на семь строк жирным 21px. На
                    // телефонах так уже сделано — теперь одинаково везде.
                    3 => {
                        ui.set_vpn_state(3);
                        ui.set_vpn_status("Не удалось подключиться".into());
                        ui.set_vpn_sub(if err.is_empty() { "Попробуйте ещё раз или выберите другой хост.".into() } else { err.into() });
                    }
                    // ХОСТ ЗАВЕРШИЛ РАЗДАЧУ. Состояние — обычное «выключено» (0), то
                    // есть спокойный синий круг, а не красная карточка отказа:
                    // ошибки здесь нет, всё сработало правильно. Меняются только
                    // слова — вместо «VPN выключен» человек читает, что случилось и
                    // что делать дальше.
                    4 => {
                        ui.set_vpn_state(0);
                        ui.set_vpn_host_id("".into());
                        ui.set_vpn_status("Хост завершил раздачу".into());
                        ui.set_vpn_sub("Выберите другой хост или нажмите «Старт»".into());
                    }
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
    let _ = std::fs::remove_dir(&up_dir);

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

/// Сохранить настройки раздачи и адрес координатора в конфиг ОС.
///
/// До этого во всём GUI не было ни одного `Config::save()`: имя хоста, лимит
/// гостей, пароль, протокол, видимость И ВЫБРАННЫЙ КООРДИНАТОР жили только в
/// памяти окна и сбрасывались при каждом запуске — человек заново вбивал свой
/// сервер после перезагрузки. Терминальная оболочка это умела с самого начала
/// (tui.rs::persist), пишем в тот же файл теми же полями, чтобы обе оболочки
/// видели одни настройки.
///
/// Права 0600 на файл ставит сам `Config::save` — в нём лежит пароль раздачи.
fn save_config(engine: &EngineSlot, ui: &AppWindow) {
    let mut cfg = engine.lock().unwrap().config().clone();
    cfg.host.name = ui.get_host_name().to_string();
    cfg.host.public = ui.get_host_public();
    cfg.host.max_guests = ui.get_host_max().max(1) as u32;
    cfg.host.password = ui.get_host_password().to_string();
    let proto = ui.get_host_protocol().to_string();
    if !proto.is_empty() {
        cfg.default_protocol = proto;
    }
    // Координатор в конфиг движка кладёт wire_coord, отдельно его тут не берём —
    // иначе показанный доменом адрес мог бы уехать в конфиг без схемы.
    if let Err(e) = cfg.save() {
        ui.set_config_error(format!("Настройки не сохранились: {e}").into());
    }
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

    // Аргумент — ID ХОСТА, а не номер строки. Номер врал: список пересобирается
    // целиком на каждое обновление каталога (а оно приходит сразу, как только у
    // любого хоста сменился счётчик гостей), состав меняется, и замер продолжал
    // мерить хост, который на этом месте стоял РАНЬШЕ.
    ui.on_expanded_host(move |id| {
        use std::sync::atomic::Ordering;
        // Увеличиваем ВСЕГДА — это и есть отмена предыдущего цикла.
        let mine = gen.fetch_add(1, Ordering::SeqCst) + 1;

        // Пустая строка приходит при сворачивании: мерить больше нечего.
        if id.is_empty() {
            return;
        }
        let Some(h) = hosts.lock().unwrap().iter().find(|h| h.id == id.as_str()).cloned() else { return };

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

/// Кого показывать в списке (как displayedHosts на iOS).
///
/// Правила три, и каждое появилось из живой жалобы:
/// * онлайн — очевидное;
/// * тот, к кому мы ПОДКЛЮЧЕНЫ, — даже если координатор уже пометил его
///   офлайном: исчезнувшая строка при работающем VPN читается как «оборвалось»;
/// * свой хост, ПОКА раздаём, — а после остановки его надо ПРЯТАТЬ: запись в
///   каталоге живёт ещё несколько секунд после bye, и человек видел «призрак»
///   собственной раздачи, который уже никого не пускает.
fn visible_hosts(list: &[HostInfo], connected_to: &str, my_code: &str, hosting: bool) -> Vec<HostInfo> {
    list.iter()
        .filter(|h| {
            let mine = !my_code.is_empty() && h.id == my_code;
            if mine {
                return hosting; // своя запись видна ровно пока раздаём
            }
            h.online || h.id == connected_to
        })
        .cloned()
        .collect()
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
    // Подпись под именем. В свёрнутой строке места мало и она ужимается
    // многоточием с ХВОСТА, поэтому здесь остаётся только то, ради чего на
    // строку смотрят.
    //
    // СЫРОГО IP тут больше нет. Он стоял первым (когда страна не определилась)
    // и один занимал всю ширину — до счётчика гостей, самого полезного в
    // строке, многоточие просто не доходило. Увидеть адрес можно в раскрытой
    // карточке: там под него отдельная плитка, и она копируется тапом.
    //
    // Счётчик гостей есть ВСЕГДА — это и делает подпись непустой при любых
    // данных: страна может не определиться, адрес может не прийти, связь с
    // координатором может оборваться (тогда список остаётся прошлый, только
    // приглушённый), а «гостей …» останется на месте.
    let sub = {
        let mut parts: Vec<String> = Vec::new();
        if let Some(c) = &cc { parts.push(c.clone()); }
        // Потолок хост может и не объявить (0). Дробь «1/0» в этом случае врёт
        // — честнее без знаменателя.
        parts.push(if h.max_guests > 0 {
            format!("гостей {}/{}", h.guests, h.max_guests)
        } else {
            format!("гостей {}", h.guests)
        });
        parts.join(" · ")
    };
    let fill = if h.max_guests > 0 { (h.guests as f32 / h.max_guests as f32).clamp(0.0, 1.0) } else { 0.0 };
    HostRow {
        id: h.id.clone().into(),
        name: display_name(h).into(),
        flag_img: flag_img.clone().unwrap_or_default(),
        has_flag: flag_img.is_some(),
        subtitle: sub.into(),
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
            let alive = eng.coordinator_health().await.is_ok();
            // Настоящий круг до координатора. Раньше здесь замеряли, СКОЛЬКО
            // длился сам вызов health(), — а он читает флаг «сокет жив» и
            // возвращается мгновенно, поэтому на экране вечно стоял ноль.
            let ping = eng.coordinator_rtt().unwrap_or(0) as i32;
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

            let got_catalog = list.is_some();
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
                let hosting = ui.get_host_state() == 2 || ui.get_host_state() == 1;
                let visible = visible_hosts(&list, &ui.get_vpn_host_id(), &ui.get_host_code(), hosting);
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
            // Пауза нужна на ЛЮБОЙ неудаче, а не только когда сокет объявлен
            // мёртвым. Watch может вернуть ошибку при живом сокете (сервер
            // ответил 5xx, снапшот не собрался) — и тогда цикл крутился без
            // паузы вообще, выдавая сотни запросов в секунду в чужой сервер.
            if !alive || !got_catalog {
                tokio::time::sleep(Duration::from_secs(2)).await;
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

    // Подключение по ID карточки.
    //
    // Раньше сюда приходил НОМЕР СТРОКИ, а список параллельно заменяет фоновая
    // задача каталога: между нажатием и обработкой состав мог смениться, и
    // человек подключался не к тому хосту, на который нажал. Цена ошибки здесь
    // — весь трафик через чужую машину, поэтому индексов тут больше нет.
    {
        let weak = ui.as_weak();
        let (hosts, hlp) = (hosts.clone(), hlp.clone());
        let coord_full = coord_full.clone();
        ui.on_connect(move |id| {
            let Some(u) = weak.upgrade() else { return };
            if busy(&u) { return; }
            let Some(host) = hosts.lock().unwrap().iter().find(|h| h.id == id.as_str()).cloned() else { return };
            if u.get_host_state() == 2 && u.get_host_code() == host.id.as_str() {
                u.set_vpn_state(3);
                u.set_vpn_status("Не удалось подключиться".into());
                u.set_vpn_sub("Это ваш собственный хост — выберите другой.".into());
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
                u.set_vpn_status("Не удалось подключиться".into());
                u.set_vpn_sub("Это код вашего же хоста — введите чужой.".into());
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
            // Порядок перебора — общий с терминалом (bmv_desktop::tunnel), чтобы
            // «Старт» вёл в один и тот же хост в обеих оболочках. Страну хоста
            // здесь определяем по IP встроенной базой, а не по самоотчёту.
            let cands = {
                let hs = hosts.lock().unwrap();
                bmv_desktop::tunnel::rank_candidates(&hs, mine.as_deref(), &own, &host_cc)
            };
            let Some(first) = cands.first() else {
                u.set_vpn_state(3);
                u.set_vpn_status("Не удалось подключиться".into());
                u.set_vpn_sub("Сейчас нет свободного хоста без пароля. Выберите хост в списке ниже.".into());
                return;
            };
            begin(&u, &first.id, &display_name(first));
            let list: Vec<(String, String)> = cands.iter()
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
    let engine_ap = engine.clone(); // для «Применить» (сохранение настроек)
    let engine_tg = engine.clone(); // для сохранения настроек при старте раздачи

    let handle_nc = handle.clone();
    let hs_nc = host_started.clone();
    let weak = ui.as_weak();
    let ht = host_task.clone();
    let heng = host_engine.clone();
    let byhandle = handle.clone();
    let hs = host_started.clone();
    let my_ip_toggle = my_ip.clone();
    ui.on_toggle_host(move || {
        let state = weak.upgrade().map(|u| u.get_host_state()).unwrap_or(0);
        if state == 1 || state == 2 {
            // СНАЧАЛА рвём дерево задач раздачи, потом прощаемся с каталогом.
            // Внутри задачи живёт heartbeat, который каждые 10с чинит запись:
            // отправь мы bye первым — heartbeat мог бы воскресить хост-призрак.
            if let Some(h) = ht.borrow_mut().take() { h.abort(); }
            // Движок снимается в ЛОКАЛЬНУЮ до ветвления: guard мьютекса в
            // скрутинии `if let` жил бы через всё тело (edition 2021), а тело
            // тут запускает чужой код.
            let eng = heng.lock().unwrap().take();
            if let Some(eng) = eng {
                byhandle.spawn(async move { let _ = tokio::time::timeout(Duration::from_secs(3), eng.host_deannounce()).await; });
            }
            *hs.lock().unwrap() = None;
            set_host(&weak, 0, "Раздача выключена".into(), String::new());
            return;
        }
        let (name, public, max, password, protocol) = weak.upgrade()
            .map(|u| (u.get_host_name().to_string(), u.get_host_public(), u.get_host_max().max(1) as u32,
                      u.get_host_password().to_string(), u.get_host_protocol().to_string()))
            .unwrap_or_else(|| (String::new(), true, 8, String::new(), "noise".into()));
        // Имя по умолчанию — СТРАНА по своему IP. Считаем ЗДЕСЬ, потому что
        // раньше подстановка жила только в «Применить»: старт раздачи с пустым
        // полем отправлял хост в ПУБЛИЧНЫЙ каталог вообще без имени, и в списке
        // у всех он показывался голым кодом.
        let fallback_name = default_host_name(&my_ip_toggle.lock().unwrap().clone());
        let base = engine.lock().unwrap().config().clone();
        let (weak, heng2, hs2) = (weak.clone(), heng.clone(), hs.clone());
        if let Some(ui) = weak.upgrade() {
            save_config(&engine_tg, &ui); // раздача пошла с этими настройками — они и сохраняются
            ui.set_host_state(1);
            ui.set_host_status("Запускаюсь…".into());
            ui.set_host_error("".into());
            // КОД НЕ СТИРАЕМ. Он выдан сервером, валиден и переживает
            // перезапуск. Стирали его здесь, а восстанавливать было некому:
            // после неудачного старта (например, за NAT — 422) код исчезал с
            // экрана до перезапуска приложения, хотя комментарий в set_host_err
            // утверждал обратное.
        }
        let task = handle.spawn(async move {
            let build = |id: &str, sig: &str| {
                let mut cfg = base.clone();
                cfg.host.name = if name.trim().is_empty() { fallback_name.clone() } else { name.clone() };
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
                    _ => return set_host_err(&weak, "Сервер не выдал код сети. Проверьте связь и попробуйте ещё раз.".into()),
                }
            }
            let mut eng = Arc::new(BmvEngine::from_config(build(&id, &sig)));
            let mut announce = eng.host_bind_announce().await;
            if announce.as_ref().err().and_then(|e| e.refusal_code()) == Some(403) {
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
                    // Причину смотрим ПО КОДУ, а не по буквам в тексте: код
                    // приезжает отдельным полем (см. `Error::Refused`). Раньше
                    // здесь стоял `s.contains("422")` — и не срабатывал никогда.
                    let msg = if e.refusal_code() == Some(422) {
                        "Ваша сеть не пропускает гостей внутрь — раздавать отсюда не выйдет. Попробуйте другую сеть.".to_string()
                    } else {
                        e.to_string()
                    };
                    return set_host_err(&weak, msg);
                }
            };
            *heng2.lock().unwrap() = Some(eng.clone());
            *hs2.lock().unwrap() = Some(std::time::Instant::now());
            set_host(&weak, 2, "Раздаю интернет".into(), eng.host_id().to_string());

            // Всё дерево раздачи — в bmv_desktop::hosting: там сессии гостей
            // порождаются в тот же JoinSet, поэтому abort() этой задачи гасит и
            // УЖЕ ПОДКЛЮЧЁННЫХ гостей. Раньше сессии были отдельными spawn'ами и
            // после «Выключить» продолжали ходить в интернет через эту машину.
            bmv_desktop::hosting::serve_host(eng.clone(), hub).await;
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
            // Тот же порядок, что и в «Выключить»: сперва оборвать дерево задач
            // (в нём heartbeat), потом bye. И тот же срез движка в локальную —
            // guard `if let` жил бы через всё тело ветки.
            if let Some(h) = ht2.borrow_mut().take() { h.abort(); }
            let eng = heng2.lock().unwrap().take();
            if let Some(eng) = eng {
                byh.spawn(async move { let _ = tokio::time::timeout(Duration::from_secs(3), eng.host_deannounce()).await; });
            }
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
            let Some(u) = weak.upgrade() else { return };
            // Настройки сохраняем ВСЕГДА — и когда раздача ещё не запущена.
            // Иначе имя, лимит, пароль, протокол и видимость жили бы только в
            // памяти окна и терялись при выходе.
            save_config(&engine_ap, &u);
            // Движок раздачи — в ЛОКАЛЬНУЮ до ветвления: guard мьютекса из
            // скрутинии `let … else` живёт до конца ВСЕГО оператора.
            let eng = heng2.lock().unwrap().clone();
            let Some(eng) = eng else { return }; // не раздаём — сохранили и хватит
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
    // Схему отделяем ДО обрезки хвостовых слэшей. Иначе одинокое «https://»
    // (человек стёр адрес, оставив схему) превращалось в «https:» и получало
    // ВТОРУЮ схему спереди — «https://https:», и приложение честно уходило
    // искать координатор по этому адресу.
    let t = input.trim();
    let (scheme, rest) = match (t.strip_prefix("https://"), t.strip_prefix("http://")) {
        (Some(r), _) => ("https://", r),
        (_, Some(r)) => ("http://", r),
        _ => ("https://", t),
    };
    format!("{scheme}{}", rest.trim_end_matches('/'))
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
                // Выбранный сервер обязан пережить перезапуск: раньше человек
                // вбивал свой адрес заново после каждого запуска приложения.
                save_config(&engine, &ui);
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
    /// Ширина ВСЕГДА проектная, высота — четыре пятых экрана вместе с заголовком.
    ///
    /// Регрессия на «окно выглядит приближенным»: ширину выводили из высоты по
    /// пропорции 400:820, и на экране в 800 точек выходило 273 точки вместо 400
    /// — макет, свёрстанный под 400, ужимался в две трети, а кегли и значки
    /// заданы в точках и не ужимались вместе с ним.
    ///
    /// По высоте меряем то, что ВИДНО на экране: заголовок рисует ОС поверх
    /// запрошенной высоты, и без этой поправки окно всегда выше заказанного.
    #[test]
    fn window_is_always_layout_wide_and_fits_the_screen_in_height() {
        const TITLE_BAR: f32 = 40.0;

        // Реальные экраны: ноутбук 800, привычный 1080, Retina 1107, большой 1440.
        for screen in [800.0_f32, 1080.0, 1107.0, 1440.0] {
            let on_screen = fit_height(screen) + TITLE_BAR;
            let share = on_screen / screen;
            assert!(
                (share - 0.80).abs() < 0.02,
                "экран {screen}: окно занимает {on_screen} — это {share} экрана, а не четыре пятых",
            );
            // Пятая часть экрана остаётся системным панелям и воздуху.
            assert!(screen - on_screen > 0.15 * screen, "экран {screen}: слишком тесно");
        }

        // Ширина от экрана НЕ зависит вообще — она свойство макета.
        assert_eq!(window_size().0, LAYOUT_W);
        assert_eq!(LAYOUT_W, 400.0);

        // На крошечном экране долю нарушаем: лучше вылезти, чем показать щель.
        assert_eq!(fit_height(400.0), 520.0);
    }

    /// Высота экрана приводится к ЛОГИЧЕСКИМ точкам — и на Windows тоже.
    ///
    /// Регрессия на баг, из-за которого на Windows окно выходило ВЫШЕ экрана:
    /// `display-info` отдаёт там физические пиксели (`dmPelsHeight`), а на
    /// macOS/Linux — точки. Раньше мы одинаково брали `height` как точки, и при
    /// масштабе 150% просили полное окно выше всего экрана.
    ///
    /// Тест гоняет ОБЕ ветки на любой машине: единицы задаются аргументом.
    #[test]
    fn screen_height_is_measured_in_logical_points_on_every_os() {
        const TITLE_BAR: f32 = 40.0;

        // macOS/Linux: крейт уже отдал точки — трогать их нельзя. Retina 2214
        // физических пикселей приходят как 1107 точек при масштабе 2.
        assert_eq!(logical_screen_h(1107.0, 2.0, false), 1107.0);
        assert_eq!(logical_screen_h(800.0, 1.0, false), 800.0);

        // Windows: пришли пиксели — переводим в точки по масштабу.
        assert_eq!(logical_screen_h(1080.0, 1.5, true), 720.0); // FullHD @ 150%
        assert_eq!(logical_screen_h(2160.0, 2.0, true), 1080.0); // 4K @ 200%
        assert_eq!(logical_screen_h(1920.0, 1.0, true), 1920.0); // без масштаба

        // Мусорный масштаб не должен превращаться в бесконечное окно.
        for bad in [0.0_f32, -1.0, f32::NAN, f32::INFINITY] {
            assert_eq!(logical_screen_h(1080.0, bad, true), 1080.0, "масштаб {bad}");
        }

        // Главное: на реальных конфигурациях Windows окно ЦЕЛИКОМ помещается на
        // экран, да ещё с запасом на панель задач. Именно это и ломалось —
        // проверяем результат, а не деление.
        for (pixels, scale) in [(1080.0_f32, 1.5_f32), (1080.0, 1.25), (2160.0, 2.0), (1440.0, 1.0), (768.0, 1.0)] {
            let screen = logical_screen_h(pixels, scale, true);
            let on_screen = fit_height(screen) + TITLE_BAR;
            assert!(
                on_screen <= screen - 0.15 * screen,
                "экран {pixels}px @{scale}: это {screen} точек, а окно просит {on_screen}",
            );
            // …а ширина от экрана не зависит вообще.
            assert_eq!(LAYOUT_W, 400.0);
        }
    }

    #[test]
    fn address_loses_its_scheme_on_screen_but_keeps_it_in_use() {
        assert_eq!(pretty_url("https://bemyvpn.net"), "bemyvpn.net");
        assert_eq!(pretty_url("http://10.0.0.5:3330"), "10.0.0.5:3330");
        assert_eq!(pretty_url("https://bemyvpn.net/"), "bemyvpn.net");

        assert_eq!(full_url("bemyvpn.net"), "https://bemyvpn.net");
        assert_eq!(full_url("  bemyvpn.net/ "), "https://bemyvpn.net");
        // Явную схему не трогаем — в том числе незашифрованную.
        assert_eq!(full_url("http://10.0.0.5:3330"), "http://10.0.0.5:3330");
        // Одна схема без адреса адресом НЕ становится — раньше отсюда выходило
        // «https://https:», и приложение уходило искать координатор по нему.
        assert_eq!(full_url("https://"), "https://");
        assert_eq!(full_url("  "), "https://");

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

    /// ХОСТ БЕЗ ОБЪЯВЛЕННОГО ПРОТОКОЛА — ЭТО «ОБЫЧНЫЙ», А НЕ «БЕЗ ШИФРА».
    ///
    /// Он поднимает шифрованный канал (noise по умолчанию), но подписывался как
    /// незашифрованный — самый опасный вид ошибки в подписи: человек отказывался
    /// от безопасного хоста или, наоборот, переставал верить надписи вообще.
    /// Тот же вход в терминале давал «Обычный» — оболочки противоречили друг другу.
    #[test]
    fn a_host_that_did_not_name_its_protocol_is_not_called_unencrypted() {
        assert_eq!(proto_name(""), "Обычный");
        assert_eq!(proto_name("noise"), "Обычный");
        assert_eq!(proto_name("noise-aes"), "Обычный");
        assert_eq!(proto_name("noise-obfs"), "Маскировка");
        // «Без шифра» остаётся ровно за тем протоколом, который и правда без шифра.
        assert_eq!(proto_name("plain"), "Без шифра");
        // Незнакомое имя показываем как есть: врать про него мы тоже не вправе.
        assert_eq!(proto_name("wireguard"), "wireguard");
        // И терминальная оболочка обязана говорить о том же самом то же самое.
        for p in ["", "noise", "noise-obfs", "plain"] {
            assert!(
                bmv_cli_proto_agrees(p, proto_name(p)),
                "оболочки расходятся в подписи протокола «{p}»",
            );
        }
    }

    /// Что о протоколе говорит терминал (tui.rs::proto_short без эмодзи).
    fn bmv_cli_proto_agrees(p: &str, gui: &str) -> bool {
        let cli = match p {
            "noise-obfs" => "Маскировка",
            "plain" => "Без шифра",
            "" | "noise" => "Обычный",
            other => other,
        };
        cli == gui
    }

    #[test]
    fn session_clock_reads_like_a_clock() {
        assert_eq!(uptime_text(0), "00:00");
        assert_eq!(uptime_text(9), "00:09");
        assert_eq!(uptime_text(75), "01:15");
        assert_eq!(uptime_text(3599), "59:59");
        // После часа появляется разряд часов — и минуты остаются двузначными.
        assert_eq!(uptime_text(3600), "1:00:00");
        assert_eq!(uptime_text(3661), "1:01:01");
        assert_eq!(uptime_text(36_000), "10:00:00");
    }

    fn h(id: &str, online: bool) -> HostInfo {
        HostInfo { id: id.into(), online, ..HostInfo::default() }
    }

    /// Список показывает живых, текущий хост и свою раздачу — и ПРЯЧЕТ
    /// собственного «призрака» после остановки.
    ///
    /// Призрак — это своя же запись, которая живёт в каталоге ещё несколько
    /// секунд после bye: человек останавливал раздачу и продолжал видеть в
    /// списке свой хост, который уже никого не пускает.
    #[test]
    fn the_list_hides_ones_own_ghost_but_keeps_the_host_we_are_riding() {
        let list = vec![h("ЖИВОЙ", true), h("МЁРТВЫЙ", false), h("МОЙ", true)];

        // Раздаём: свой хост виден.
        let ids: Vec<String> =
            visible_hosts(&list, "", "МОЙ", true).iter().map(|x| x.id.clone()).collect();
        assert_eq!(ids, ["ЖИВОЙ", "МОЙ"]);

        // Раздачу остановили: своя запись ещё в каталоге, но показывать её нельзя.
        let ids: Vec<String> =
            visible_hosts(&list, "", "МОЙ", false).iter().map(|x| x.id.clone()).collect();
        assert_eq!(ids, ["ЖИВОЙ"]);

        // Хост, к которому мы ПОДКЛЮЧЕНЫ, остаётся видимым даже помеченный
        // офлайном — иначе строка исчезает при работающем VPN.
        let ids: Vec<String> =
            visible_hosts(&list, "МЁРТВЫЙ", "", false).iter().map(|x| x.id.clone()).collect();
        assert_eq!(ids, ["ЖИВОЙ", "МЁРТВЫЙ", "МОЙ"]);
        // …а офлайновый чужой хост, к которому мы НЕ подключены, не показываем.
        let ids: Vec<String> =
            visible_hosts(&list, "ЖИВОЙ", "", false).iter().map(|x| x.id.clone()).collect();
        assert_eq!(ids, ["ЖИВОЙ", "МОЙ"]);

        // Кода своего хоста ещё нет — пустая строка не должна прятать чужие.
        let ids: Vec<String> =
            visible_hosts(&list, "", "", false).iter().map(|x| x.id.clone()).collect();
        assert_eq!(ids, ["ЖИВОЙ", "МОЙ"]);
    }

    /// Путь уходит в `do shell script … with administrator privileges`, то есть
    /// исполняется ПОД ROOT. Апостроф в имени папки закрывал строку и всё, что
    /// дальше, становилось командой администратора.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_quoted_path_cannot_break_out_into_a_root_command() {
        use std::path::Path;
        let q = |p: &str| helper::sh_quote(Path::new(p));

        assert_eq!(q("/Applications/BeMyVPN.app"), "'/Applications/BeMyVPN.app'");
        assert_eq!(q("/Users/Моё имя/BeMyVPN.app"), "'/Users/Моё имя/BeMyVPN.app'");
        // Апостроф — тот самый случай: закрыть, вставить экранированный, открыть.
        assert_eq!(q("/Users/o'brien/x"), r"'/Users/o'\''brien/x'");

        // Проверяем НАСТОЯЩИМ /bin/sh — тем самым, который потом выполнит эту
        // строку под администратором. Путь обязан доехать до него ОДНИМ
        // аргументом, байт в байт, и ничего постороннего не выполниться.
        for path in [
            "/Applications/BeMyVPN.app",
            "/Users/o'brien/BeMyVPN.app",
            "/Users/Моё имя/BeMyVPN.app",
            "/tmp/a'; touch /tmp/бемивпн-вырвался; echo '",
            "/tmp/$(id -u) `id -u` \"кавычки\"",
        ] {
            let out = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(format!("printf '%s' {}", q(path)))
                .output()
                .expect("sh");
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                path,
                "путь доехал до администраторской команды искажённым",
            );
        }
        assert!(
            !std::path::Path::new("/tmp/бемивпн-вырвался").exists(),
            "подставленная команда ВЫПОЛНИЛАСЬ — это была бы дыра под root",
        );
    }
}
