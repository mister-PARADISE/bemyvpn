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

// Страна по IP переехала в общий крейт (`bmv_common::geo`, фича `geoip`): в
// двоичном крейте окна её нельзя было позвать из терминала, и тот показывал
// пустую колонку там, где окно показывало «NL».
use bmv_common::{geo, view};
use bmv_config::Config;
use bmv_core::BmvEngine;
use bmv_signal::HostInfo;
use slint::{Model, ModelRc, SharedString, VecModel, Weak};

mod flags;
mod helper;
mod store;

slint::include_modules!();

const DEFAULT_COORD: &str = "https://bemyvpn.net";

/// Заголовок неудачного подключения — ОДИН на все четыре места, где он ставится.
///
/// В справочник он не едет: `view::Vpn` состояния «не вышло» не знает вовсе —
/// на телефонах отказ живёт отдельной веткой с текстом ошибки, а не состоянием
/// VPN. Пока правило одно на одну оболочку, его место здесь; разъедется с
/// телефонами — переедет в `view` вместе с ними.
const VPN_FAILED: &str = "Не удалось подключиться";

type EngineSlot = Arc<Mutex<Arc<BmvEngine>>>;
type Ts = Arc<Mutex<Option<std::time::Instant>>>;
/// Замеры отклика: id хоста → готовая строка для плитки («24 мс» / «—» / «…»).
/// Живёт рядом с каталогом, потому что строки каталога пересобираются на каждом
/// обновлении, а измерение переживать это обязано — иначе цифра мигала бы.
/// Замеры отклика: id хоста → Some(мс) / None (не ответил). Отсутствие ключа =
/// ещё не мерили. Храним ЧИСЛО, а текст и цвет строятся из него — иначе цвет
/// зависел бы от того, как мы подписали значение.
type Pings = Arc<Mutex<std::collections::HashMap<String, Option<u32>>>>;

// Часы сеанса, подпись протокола, подпись пинга, годность хоста, имя хоста,
// страна хоста и приведение адреса координатора живут в ОБЩЕМ справочнике
// `bmv_common::view` — одном на все оболочки. Своих копий здесь больше нет:
// именно копии и расходились (пустой протокол окно звало «Обычный», телефон —
// «Без шифра»). Часовой против второй копии —
// `crates/bmv-common/tests/one_place_per_rule.rs`.

/// Как подписать хост: имя, а без имени — код.
fn display_name(h: &HostInfo) -> String {
    view::host_display_name(&h.name, &h.id).to_string()
}

/// Код страны хоста — правилом справочника (по адресу, поле анонса запасным).
fn host_cc(h: &HostInfo) -> Option<String> {
    view::host_country(&h.ip, &h.country)
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

/// ПОКАЗАТЬ СОСТОЯНИЕ VPN — ОДНИМ МЕСТОМ.
///
/// Состояние VPN живёт пятью источниками (файл-маркер хелпера, `vpn_since`,
/// свойство разметки, живая задача хелпера, id хоста в окне), и трогать их
/// ПОРЯДОК нельзя: на нём держатся прощание с хостом и честное «отключено».
/// Здесь порядок не при чём — это ПРОЕКЦИЯ, один кадр интерфейса, и собирается
/// он целиком. Стояла она тринадцатью местами по три-четыре setter'а, и одно
/// расхождение уже накопилось: подключение через колбэк хелпера оставляло под
/// заголовком подпись «к ИМЯ» от попытки, а догоняющий тик-цикл её стирал.
///
/// `host_id`: `Some("")` — забыть хост, `None` — оставить как есть. Второе
/// нужно после неудачи: пока запись жива, строка хоста остаётся в списке, даже
/// если координатор пометил его офлайном, — есть куда нажать «ещё раз».
fn show_vpn(ui: &AppWindow, state: i32, status: &str, sub: &str, host_id: Option<&str>) {
    ui.set_vpn_state(state);
    ui.set_vpn_status(status.into());
    ui.set_vpn_sub(sub.into());
    if let Some(id) = host_id {
        ui.set_vpn_host_id(id.into());
    }
}

/// Выключено — «VPN выключен» и никакого хоста. Подпись объясняет, ПОЧЕМУ
/// выключено (хост ушёл), либо пуста.
fn show_vpn_off(ui: &AppWindow, sub: &str) {
    show_vpn(ui, 0, view::vpn_text(view::Vpn::Off), sub, Some(""));
}

/// Заполнить карточку активного подключения по записи хоста. Зовётся В МОМЕНТ
/// подключения (данные уже на руках — гость выбрал этот хост) и потом на каждом
/// обновлении каталога, чтобы живые цифры (гости) не отставали.
fn fill_vpn_card(ui: &AppWindow, h: &HostInfo) {
    ui.set_vpn_ip(if h.ip.is_empty() { "—".into() } else { h.ip.clone().into() });
    ui.set_vpn_country(host_cc(h).unwrap_or_else(|| "—".into()).into());
    ui.set_vpn_guests(format!("{} / {}", h.guests, h.max_guests).into());
    ui.set_vpn_proto(view::proto_name(&h.protocol).into());
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

/// Имя запасного рисовальщика — программный растеризатор.
///
/// Он УЖЕ в бинаре: фича `renderer-software` входит в набор Slint по умолчанию,
/// доставать ничего не нужно. Строка та же, что понимает `SLINT_BACKEND`, но
/// ставим её не переменной, а `BackendSelector` (см. `main`).
const SW_BACKEND: &str = "winit-software";

/// СВОЯ метка «этот запуск — уже перезапуск на запасном рисовальщике».
///
/// Именно АРГУМЕНТ, и ни в коем случае не переменная окружения:
///
/// 1. Окружение НАСЛЕДУЕТСЯ чужими перезапусками. Обновление перезапускает нас
///    руками `cmd` (`bmv_common::update::spawn_exe_updater`: `start "" "<exe>"`),
///    и переменная переехала бы в новый процесс. Машина, один раз свалившаяся на
///    растеризатор, осталась бы на нём НАВСЕГДА — в том числе после установки
///    драйвера видеокарты. Аргументов `start` не передаёт: метка умирает вместе
///    с процессом, и следующий запуск снова пробует видеокарту.
/// 2. Предохранителем от петли раньше служила ЧУЖАЯ `SLINT_BACKEND`, а её
///    содержимое нам не подвластно. Пустая строка или неизвестное имя Slint
///    молча уводит на видеокарту (i-slint-backend-selector/lib.rs:94 и :119 —
///    «Could not load rendering backend …, fallback to default»), то есть
///    падение остаётся, а починка оказывается заранее запрещена.
const SW_FLAG: &str = "--software-renderer";

/// ОКНО ХОТЬ РАЗ НАРИСОВАЛОСЬ — и с этого мгновения перезапускать себя нельзя
/// НИКОГДА (иначе приложение воскресало бы у человека после закрытия).
///
/// Признак не наш домысел, а слово самого рисовальщика: femtovg зовёт
/// `RenderingSetup` из ПЕРВОГО `render()`, когда GL-контекст уже создан и кадр
/// пошёл (i-slint-renderer-femtovg/lib.rs:118-124). Все отказы, ради которых
/// существует запасной путь, случаются РАНЬШЕ — при создании окна
/// (`renderer.resume` → `set_opengl_context`), поэтому здесь ещё `false`.
///
/// Почему не `Window::is_visible()`: он отвечает «звали ли `show()`»
/// (i-slint-core/window.rs:1445 — `strong_component_ref.is_some()`), а `show()`
/// на winit честно отдаёт `Ok` ещё до того, как окно вообще создано.
///
/// В ЗАПАСНОМ РЕЖИМЕ признак не поднимается: программный растеризатор
/// `set_rendering_notifier` не умеет (у трейта `Renderer` умолчание —
/// `SetRenderingNotifierError::Unsupported`, i-slint-core/renderer.rs:115). Это
/// ничего не ломает — там перезапуск и так запрещён меткой `SW_FLAG`, а
/// сообщение человеку составлено так, чтобы годиться в обоих случаях.
static WINDOW_SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// ПРОБОВАТЬ ЛИ ЗАПАСНОЙ РИСОВАЛЬЩИК. Правило одно: окно НИ РАЗУ не
/// показывалось, и мы ещё не в запасном режиме.
///
/// Сверки текста ошибки со словом «OpenGL» здесь больше нет. Она пропускала
/// добрую половину отказов старта, и все они кончаются тем же — окна нет:
/// «Failed to retrieve a window handle for window we just created» и «Error
/// obtaining window handle to adjust nsview layer contents placement»
/// (i-slint-backend-winit/renderer/femtovg/glcontext.rs:151 и :198), «Error
/// initializing winit event loop» (там же lib.rs:578), «Winit backend failed to
/// find a suitable renderer» (lib.rs:763). Признак «окна не было» покрывает их
/// все разом и не зависит от чужих формулировок.
///
/// Окружения функция не видит вовсе — и не должна: чужая `SLINT_BACKEND` не
/// вправе ни запретить починку, ни разрешить воскрешение.
fn should_try_software(args: &[std::ffi::OsString], window_shown: bool) -> bool {
    !window_shown && !software_mode(args)
}

/// Метка запасного режима на месте — этот запуск уже перезапущенный.
///
/// Одно место на оба вопроса («включать ли растеризатор» и «перезапускаться ли
/// снова»): прочти их по-разному — и приложение однажды закрутится в петлю.
fn software_mode(args: &[std::ffi::OsString]) -> bool {
    args.iter().any(|a| a.to_str() == Some(SW_FLAG))
}

/// Команда, которой мы перезапускаем СЕБЯ на запасном рисовальщике.
///
/// Ни одной переменной окружения она не задаёт — в этом весь смысл, см. `SW_FLAG`.
/// Сторож — `the_fallback_mode_does_not_survive_a_restart_someone_else_makes`.
fn software_restart(
    exe: std::path::PathBuf,
    args: impl Iterator<Item = std::ffi::OsString>,
) -> std::process::Command {
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args).arg(SW_FLAG);
    cmd
}

/// МАШИНА БЕЗ ВИДЕОКАРТЫ: окно не поднялось — перезапустить себя на программном
/// растеризаторе. Возвращает `true`, если перезапуск отправлен.
///
/// Зачем вообще. Slint по умолчанию рисует видеокартой (femtovg/OpenGL). На
/// свежей Windows БЕЗ ДРАЙВЕРА видеокарты, в виртуальной машине и в части
/// сеансов удалённого рабочего стола OpenGL нет вовсе — femtovg проверяет наличие
/// `glCreateShader` и отказывается. Окна человек не видит ВООБЩЕ, а процесс молча
/// уходит с кодом 1: консоли у релиза нет (`windows_subsystem = "windows"`).
///
/// Почему ПЕРЕЗАПУСКОМ, а не переключением на месте. Переключиться в том же
/// запуске нельзя — три отдельных замка, каждый закрывает дверь сам по себе:
///   1. Отказ приходит не при выборе рисовальщика, а из УЖЕ КРУТЯЩЕГОСЯ цикла
///      событий: femtovg трогает OpenGL только в момент создания окна, а до того
///      и `AppWindow::new()` (если сам цикл событий поднялся), и `show()` честно
///      отдают `Ok`.
///   2. `slint::platform::set_platform` ставится один раз за процесс, второй
///      вызов — `AlreadySet`, сброса нет.
///   3. winit не даёт создать второй цикл событий в процессе
///      (`EventLoopError::RecreationAttempt`).
///
/// Оставался второй путь — заранее самому проверить, есть ли OpenGL. Это своя
/// копия пробы glutin (окно, пиксельный формат, контекст, `wglGetProcAddress`);
/// ошибись она в другую сторону — и машина С видеокартой поехала бы на CPU, то
/// есть чинили бы редкий случай ценой основного. Перезапуск ошибиться не может:
/// он случается только после НАСТОЯЩЕГО отказа.
///
/// Предохранитель от петли — СВОЯ метка `SW_FLAG` в аргументах: перезапущенный
/// процесс видит её и второй раз не перезапускается. Раньше эту роль играла
/// чужая `SLINT_BACKEND`, и мусор в ней выключал починку насовсем.
///
/// ЗНАЧКИ. Они у нас — вектор (`Path`, ui/icons.slint), и до Slint 1.15
/// программный растеризатор не рисовал `Path` ВОВСЕ: на месте значка оставалась
/// пустота. Владелец эту цену принял дословно — «если нету видеокарты, то пускай
/// их у него не будет, ничего страшного», — а перевод значков в картинки уже
/// делали и ОТКАТИЛИ (4dacbef). ЧИНИТЬ КАРТИНКАМИ ЗАПРЕЩЕНО И СЕЙЧАС.
///
/// С 1.15 платить не приходится: программный растеризатор научился `Path`
/// (i-slint-renderer-software, фича `path`, включена вместе со `std`). Проверено
/// на живом окне: `SLINT_BACKEND=winit-software` + `SLINT_DEBUG_PERFORMANCE`
/// говорит «Backend: software», и все три вкладки со значками. Пустота вернётся,
/// если кто-то соберёт крейт без `std`, — не повод возвращать картинки.
///
/// И ЧТО УЖЕ НЕ ТАК — чтобы не искали заново. Здесь стояло: «вкладка „Сервер“
/// роняет процесс, своей правкой не лечится». Первое было правдой, второе —
/// нет, и вина была не на списке серверов.
///
/// Падало ЛЮБОЕ исчезновение элемента из кадра, а «Сохранить и проверить» на
/// вкладке «Сервер» просто гасило разом четыре блока парящей панели
/// (`if coord-state == 1:`). Ронял не повторитель, а `Conditional`: в Slint
/// 1.14.1 `compute_dirty_regions` брал `borrow_mut` кеша геометрии и не
/// отпускал, пока считал габариты элемента (partial_renderer.rs:417 и 423), а
/// расчёт габаритов — это раскладка, и она лениво доводит `if`-блоки и
/// повторители до ума: сносит прежние поддеревья, а снос идёт за вторым
/// `borrow_mut` того же кеша (:818, «RefCell already borrowed»). На видеокарте
/// этого нет вовсе — femtovg не заворачивается в `PartialRenderer`.
///
/// Лечится ОБНОВЛЕНИЕМ: Slint 1.15 поднял расчёт габаритов выше взятия кеша
/// (upstream #9882 / #9883). Нижняя граница закреплена в Cargo.toml, сторож —
/// тест `the_software_renderer_survives_elements_vanishing_between_frames`.
/// «1.14.1 — последняя» было просто неверно: 1.15 вышла раньше разбора.
fn restart_on_software_renderer() -> bool {
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if !should_try_software(&args, WINDOW_SHOWN.load(std::sync::atomic::Ordering::SeqCst)) {
        return false;
    }
    let Ok(exe) = std::env::current_exe() else { return false };
    software_restart(exe, args.into_iter().skip(1)).spawn().is_ok()
}

/// ОТКАЗ СТАРТА ОКНА — ОДНИМ МЕСТОМ на все три канала, которыми он приходит:
/// `AppWindow::new()` (там создаётся цикл событий winit), `ui.run()` (там
/// создаётся само окно и GL-контекст) и паника (см. `arm_startup_panic_hook`).
///
/// Возвращается только если помочь не вышло: перезапуск уводит из процесса
/// НЕМЕДЛЕННО. Раньше родитель после `spawn` дочитывал `main` до конца, и на
/// машине пару секунд жили два наших процесса: остановка tokio и деструкторы
/// занимают заметное время, а работу уже делает потомок.
fn startup_failed(err: slint::PlatformError) -> Result<(), Box<dyn std::error::Error>> {
    if restart_on_software_renderer() {
        std::process::exit(0);
    }
    report_startup_failure(&err.to_string());
    Err(err.into())
}

/// ПАНИКА ДО ПЕРВОГО КАДРА — ТОТ ЖЕ ОТКАЗ СТАРТА, только другим каналом.
///
/// На пути создания окна femtovg не только возвращает ошибки, но и ПАНИКУЕТ:
/// `.expect("internal error: Could not find any matching GL configuration")`
/// (i-slint-backend-winit/renderer/femtovg/glcontext.rs:89 — glutin не отдал ни
/// одной подходящей конфигурации) и два `.unwrap()` в
/// i-slint-renderer-femtovg/opengl.rs:113 и :142 (`OpenGl::new_from_function_cstr`
/// и `Canvas::new_with_text_context`). `Result` от `ui.run()` их не видит вовсе,
/// и на Windows без консоли человек снова не видит НИЧЕГО — то есть исходная
/// жалоба остаётся как была.
///
/// Обработчик паники зовётся ДО раскрутки стека (и до `abort`, если сборку
/// когда-нибудь переведут на `panic = "abort"`), поэтому ловит и такое.
///
/// ЧУЖИЕ ПАНИКИ НЕ НАШИ: сверяем поток. Паника в задаче tokio к окну отношения
/// не имеет, и перезапускаться на ней нельзя.
///
/// Разбирается ЛЮБАЯ паника до первого кадра, не только графическая: человек в
/// любом случае остался без окна и без единого слова, а разбираться, чья именно
/// паника, по её тексту — та же сверка чужих формулировок, от которой мы
/// избавились в `should_try_software`. Худшее, что даёт лишний перезапуск, —
/// вторая такая же паника в потомке, и уже она доедет до человека сообщением.
fn arm_startup_panic_hook() {
    let main_thread = std::thread::current().id();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if std::thread::current().id() != main_thread
            || WINDOW_SHOWN.load(std::sync::atomic::Ordering::SeqCst)
        {
            previous(info);
            return;
        }
        if restart_on_software_renderer() {
            std::process::exit(0);
        }
        report_startup_failure(&info.to_string());
        // Своим выходом, а не раскруткой: показывать поверх системного окна ещё
        // и стандартный отчёт о панике (на Windows он всё равно уходит в никуда)
        // человеку незачем.
        std::process::exit(1);
    }));
}

/// СКАЗАТЬ ЧЕЛОВЕКУ, что запустить окно не вышло, — СРЕДСТВАМИ САМОЙ СИСТЕМЫ.
///
/// Своего окна для этого нет и быть не может: сообщение как раз о том, что
/// рисовать мы не умеем. Печать в поток тоже не годится — у релиза под Windows
/// консоли нет вовсе (`windows_subsystem = "windows"`), и строка уходит в
/// никуда; ровно на это и жалуются. Здесь стояло «на Unix есть консоль» — это
/// было неправдой: у `.app`, запущенного мышью, и у ярлыка на Linux её нет
/// ровно так же.
///
/// Своей печати в журнал в клиенте нет и быть не может
/// (`crates/bmv-common/tests/no_journal_in_the_client.rs`) — поэтому ниже
/// системные средства, а не `eprintln!`.
///
/// Формулировка годится и для паники ПОСЛЕ показа окна в запасном режиме (там
/// признак `WINDOW_SHOWN` не поднимается, см. его): первая строка не утверждает,
/// что окна не было, а совет отделён условием.
fn report_startup_failure(details: &str) {
    let text = format!(
        "BeMyVPN не смог продолжить работу.\n\n{details}\n\n\
         Если окно так и не появилось: похоже, на этой машине нет ни видеокарты \
         с OpenGL, ни рабочего запасного рисовальщика. Обычно помогает установка \
         драйвера видеокарты."
    );

    // Windows: системное окно. Модальное НАРОЧНО — сообщение, которое некому
    // прочесть, бессмысленно, а больше процессу делать нечего: за `MessageBoxW`
    // сразу выход. FOREGROUND и TOPMOST обязательны: родитель к этому моменту
    // уже ушёл, и без них окно всплывает ПОД чужими — то есть снова «ничего не
    // происходит».
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            MessageBoxW, MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_TOPMOST,
        };
        // Обе строки — UTF-16 с завершающим нулём: MessageBoxW читает до него.
        let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
        let (body, title) = (wide(&text), wide("BeMyVPN"));
        // SAFETY: оба указателя — на живые буферы с нулём на конце, окна-владельца
        // нет (его-то и не создалось), флаги — константы из windows-sys.
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                body.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONERROR | MB_SETFOREGROUND | MB_TOPMOST,
            )
        };
    }

    // macOS: `display alert` рисует сама система. `osascript` есть в любой
    // системе с самой её установки — доставать нечего.
    #[cfg(target_os = "macos")]
    {
        // Строковый литерал AppleScript: обратный слэш, кавычка и перевод
        // строки — единственное, что его ломает. Текст свой, но в него подставлен
        // чужой (`details`), и он бывает любым.
        let esc = text.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n");
        let _ = std::process::Command::new("/usr/bin/osascript")
            .arg("-e")
            .arg(format!("display alert \"BeMyVPN\" message \"{esc}\" as critical"))
            .status();
    }

    // Linux: своего окна сообщений у системы нет, есть три расхожие утилиты.
    // Пробуем по очереди до первой, которая нашлась. Не нашлось ни одной —
    // остаётся стандартный вывод причины возвратом `Err` из `main`.
    #[cfg(all(unix, not(target_os = "macos")))]
    for (prog, args) in [
        ("zenity", &["--error", "--title=BeMyVPN", "--text"][..]),
        ("kdialog", &["--title", "BeMyVPN", "--error"][..]),
        ("xmessage", &["-center"][..]),
    ] {
        if std::process::Command::new(prog).args(args).arg(&text).status().is_ok() {
            return;
        }
    }
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

    // ЗАПАСНОЙ РИСОВАЛЬЩИК — по СВОЕЙ метке в аргументах (см. `SW_FLAG`), а не
    // по `SLINT_BACKEND`. `BackendSelector` переменную ПЕРЕБИВАЕТ: имя
    // разбирается на бэкенд и рисовальщик сразу (`winit` + `software`), а ветка
    // чтения `SLINT_BACKEND` в i-slint-backend-selector/api.rs:279 работает
    // только когда что-то из двух не задано. Поэтому чужой мусор в переменной
    // наш запасной путь не ломает.
    if software_mode(&std::env::args_os().collect::<Vec<_>>()) {
        if let Err(e) = slint::BackendSelector::new().backend_name(SW_BACKEND.to_string()).select()
        {
            return startup_failed(e);
        }
    }

    // Ловушку на панику ставим ЗДЕСЬ: ниже начинается всё, что имеет отношение к
    // окну, — создание цикла событий (`AppWindow::new`) и потом самого окна
    // (`ui.run`). Выше неё паника означала бы что-то другое, и разбирать её как
    // отказ рисовальщика было бы враньём.
    arm_startup_panic_hook();

    // ОТКАЗ ЗДЕСЬ ТОЖЕ РАЗБИРАЕМ. Внутри `AppWindow::new()` создаётся цикл
    // событий winit (сгенерированный `new()` зовёт `window_adapter_ref()`, тот —
    // `create_window_adapter`, а он поднимает бэкенд целиком, включая
    // `EventLoop::build`, i-slint-backend-winit/lib.rs:578). Раньше эта ошибка
    // уходила через `?` — мимо и запасного пути, и человека.
    let ui = match AppWindow::new() {
        Ok(ui) => ui,
        Err(e) => return startup_failed(e),
    };
    // «ОКНО ПОКАЗАЛОСЬ» — со слов самого рисовальщика, см. `WINDOW_SHOWN`.
    // Программный растеризатор такого не умеет и отвечает `Unsupported` — там
    // признак и не нужен (перезапуск запрещён меткой).
    let _ = ui.window().set_rendering_notifier(|_, _| {
        WINDOW_SHOWN.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    {
        let (w, h) = window_size();
        ui.set_win_w(w);
        ui.set_win_h(h);
        // Тянуть окно мышью нельзя НИГДЕ: min = max = этот размер (см. app.slint).
        // Исключение для Windows было и ОТМЕНЕНО владельцем 04.08 — вид один на
        // всех ОС.
    }
    ui.set_coord_url(view::without_scheme(&coord).into());
    ui.set_coord_field(view::without_scheme(&coord).into());
    ui.set_coord_is_default(coord == DEFAULT_COORD);
    ui.set_config_error(config_error.into());
    // Подписи состояния VPN — ТОЛЬКО из справочника (`view::vpn_text`), и
    // проекция в разметку — тоже одним местом (`show_vpn*`). Здесь эти строки
    // раздавались из одиннадцати мест сразу.
    show_vpn_off(&ui, "");
    // Сохранённые настройки раздачи — на экран.
    ui.set_host_name(saved_host.name.into());
    ui.set_host_public(saved_host.public);
    ui.set_host_max(saved_host.max_guests.clamp(1, i32::MAX as u32) as i32);
    ui.set_host_password(saved_host.password.into());
    if !saved_proto.is_empty() {
        ui.set_host_protocol(saved_proto.into());
    }
    ui.set_server_history(str_model(
        &store::load_server_history().iter().map(|u| view::without_scheme(u)).collect::<Vec<_>>(),
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
                    1 => show_vpn(&ui, 1, view::vpn_text(view::Vpn::Connecting), &format!("к {name}"), Some(&id)),
                    2 => {
                        show_vpn(&ui, 2, &name, "", Some(&id));
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
                    // Запись хоста НЕ гасим — см. `show_vpn`.
                    3 => show_vpn(
                        &ui,
                        3,
                        VPN_FAILED,
                        if err.is_empty() { "Попробуйте ещё раз или выберите другой хост." } else { &err },
                        None,
                    ),
                    // ХОСТ ЗАВЕРШИЛ РАЗДАЧУ. Состояние — обычное «выключено» (0), то
                    // есть спокойный синий круг, а не красная карточка отказа:
                    // ошибки здесь нет, всё сработало правильно. Меняются только
                    // слова — вместо «VPN выключен» человек читает, что случилось и
                    // что делать дальше.
                    //
                    // ОБЕ ФРАЗЫ — ИЗ СПРАВОЧНИКА, и обе целиком: заголовок
                    // говорит, что VPN выключен, подпись — почему и что делать.
                    // Своей формулировки («Хост завершил раздачу» + «Выберите
                    // другой хост или нажмите «Старт»») здесь больше нет.
                    //
                    // ПОЧЕМУ ОБЪЯСНЕНИЕ ВНИЗУ, А НЕ В ЗАГОЛОВКЕ: заголовок —
                    // 21px жирным, и на настоящем окне (400 точек ширины) фраза
                    // правила в него не влезала. Подпись же переносится по
                    // словам. Тот же порядок здесь уже принят для отказа:
                    // короткий заголовок, объяснение строкой ниже.
                    4 => show_vpn_off(&ui, view::vpn_text(view::Vpn::Ended)),
                    _ => show_vpn_off(&ui, ""),
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

    // Окно закрыто — или НЕ ОТКРЫЛОСЬ ВОВСЕ (машина без видеокарты). Второй
    // случай разбирает `startup_failed`: молча уходить с кодом 1 нельзя, консоли
    // у релиза нет.
    if let Err(e) = ui.run() {
        // Каталог обмена убираем САМИ: при удачном перезапуске `startup_failed`
        // уходит из процесса немедленно, а `std::process::exit` деструкторов не
        // зовёт — иначе во временной папке копился бы мусор.
        drop(up_dir);
        return startup_failed(e);
    }
    // Маркер и его каталог уберёт `Drop` у `up_dir` (см. `helper::PrivateDir`) —
    // а на пути отказа выше он снят вручную, потому что оттуда мы уходим
    // `std::process::exit`, минуя деструкторы.

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
                let (text, alarm) = view::ping(ms);
                r.ping = text.into();
                r.ping_alarm = alarm as i32;
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
    let (ping_text, ping_alarm) = {
        let map = pings.lock().unwrap();
        match map.get(&h.id) {
            // Подпись И уровень тревоги — ОДНИМ вызовом справочника: порознь они
            // и разъезжались (текст брали в одном месте, цвет считали в другом).
            // Разметке уезжает уровень, а не миллисекунды: линейка живёт в
            // справочнике, окну достаточно знать, насколько цифра тревожна.
            Some(measured) => {
                let (text, alarm) = view::ping(*measured);
                (SharedString::from(text), alarm as i32)
            }
            // Ключа нет — ещё не мерили: пусто и −1, у плитки на это своя
            // подпись ожидания. Справочник такого состояния не знает.
            None => (SharedString::new(), -1),
        }
    };
    let usable = view::host_usable(h.online, h.guests, h.max_guests);
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
    //
    // КОДА СТРАНЫ ТУТ БОЛЬШЕ НЕТ. Он стоял первым («NL · гостей 0/32») и
    // повторял флаг, который лежит слева на плашке в трёх сантиметрах от него:
    // одна и та же страна дважды в одной строке. Флаг остаётся, а место уходит
    // счётчику. Полный код виден в раскрытой карточке, в плитке «СТРАНА».
    //
    // Потолок хост может и не объявить (0). Дробь «1/0» в этом случае врёт —
    // честнее без знаменателя.
    let sub = if h.max_guests > 0 {
        format!("гостей {}/{}", h.guests, h.max_guests)
    } else {
        format!("гостей {}", h.guests)
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
        proto: view::proto_name(&h.protocol).into(),
        proto_id: h.protocol.clone().into(),
        // Пусто до раскрытия карточки: мерить отклик для строк, на которые никто
        // не смотрит, — зря дёргать чужие машины.
        ping: ping_text,
        ping_alarm,
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
                let vpn_el = vpn_since.lock().unwrap().map(|t| view::session_clock(t.elapsed().as_secs()));
                let host_el = host_started.lock().unwrap().map(|t| view::session_clock(t.elapsed().as_secs()));
                let (weak2, names) = (weak.clone(), hosts.clone());
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak2.upgrade() else { return };
                    // Туннель поднят, а UI застрял на «Подключаюсь…» → догоняем.
                    if let Some(id) = &helper_up {
                        if ui.get_vpn_state() == 1 {
                            let name = {
                                let hs = names.lock().unwrap();
                                if let Some(h) = hs.iter().find(|h| &h.id == id) {
                                    fill_vpn_card(&ui, h); // догнали статус — догоняем и ячейки
                                    display_name(h)
                                } else {
                                    id.clone()
                                }
                            };
                            show_vpn(&ui, 2, &name, "", Some(id));
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
                        show_vpn_off(&ui, "");
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
                r = eng.guest_watch(None, ver) => Some(r),
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
            let ping = eng.coordinator_rtt().unwrap_or(0);
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

                // Подпись и уровень тревоги — ОДНИМ вызовом справочника, той же
                // линейкой, что у пинга до хоста. Разметка раньше сама клеила
                // «мс» и сама сравнивала число с порогами — две копии правила
                // на одном экране.
                let (ping_text, ping_alarm) = view::ping(alive.then_some(ping));
                ui.set_coord_ping(ping_text.into());
                ui.set_coord_ping_alarm(ping_alarm as i32);
                ui.set_coord_ip(ip.into());
                if let Some(h) = hist {
                    ui.set_server_history(str_model(&h.iter().map(|u| view::without_scheme(u)).collect::<Vec<_>>()));
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
        show_vpn(ui, 1, view::vpn_text(view::Vpn::Connecting), &format!("к {name}"), Some(id));
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
                show_vpn(&u, 3, VPN_FAILED, "Это ваш собственный хост — выберите другой.", None);
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
                show_vpn(&u, 3, VPN_FAILED, "Это код вашего же хоста — введите чужой.", None);
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
                show_vpn(&u, 3, VPN_FAILED, "Сейчас нет свободного хоста без пароля. Выберите хост в списке ниже.", None);
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
                show_vpn_off(&u, "");
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
            set_host(&weak, 0, String::new());
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
                        "Не удалось определить ваш адрес в интернете — без него гости не найдут дорогу. Попробуйте ещё раз, а если не проходит — из другой сети.".to_string()
                    } else {
                        e.to_string()
                    };
                    return set_host_err(&weak, msg);
                }
            };
            *heng2.lock().unwrap() = Some(eng.clone());
            *hs2.lock().unwrap() = Some(std::time::Instant::now());
            set_host(&weak, 2, eng.host_id().to_string());

            // Всё дерево раздачи — в bmv_desktop::hosting: там сессии гостей
            // порождаются в тот же JoinSet, поэтому abort() этой задачи гасит и
            // УЖЕ ПОДКЛЮЧЁННЫХ гостей. Раньше сессии были отдельными spawn'ами и
            // после «Выключить» продолжали ходить в интернет через эту машину.
            bmv_desktop::hosting::serve_host(eng.clone(), hub).await;
            heng2.lock().unwrap().take();
            *hs2.lock().unwrap() = None;
            set_host(&weak, 0, String::new());
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
            set_host(&weak, 0, String::new());
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

fn set_host(weak: &Weak<AppWindow>, state: i32, code: String) {
    let weak = weak.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_host_state(state);
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
            // Код НЕ сбрасываем — он валиден, пригодится для повторной попытки.
            ui.set_host_error(msg.into());
        }
    });
}

// ── Сервер (смена координатора) ──────────────────────────────────────────────

// Показ адреса без схемы (`view::without_scheme`) и обратное приведение
// набранного (`view::coordinator_url`) — тоже в общем справочнике: телефоны на
// том же вводе молча выходили из настройки, ничего не сохранив.

fn wire_coord(ui: &AppWindow, engine: EngineSlot, coord_full: Arc<Mutex<String>>) {
    let apply = {
        let engine = engine.clone();
        let weak = ui.as_weak();
        move |url: String| {
            // Пустой ввод — отказ: раньше здесь стояло `url.len() <= "https://".len()`,
            // и заодно отвергался годный «http://x».
            let Some(url) = view::coordinator_url(&url) else { return };
            *coord_full.lock().unwrap() = url.clone();
            let mut cfg = engine.lock().unwrap().config().clone();
            cfg.coordinators = vec![url.clone()];
            *engine.lock().unwrap() = Arc::new(BmvEngine::from_config(cfg));
            if let Some(ui) = weak.upgrade() {
                ui.set_coord_is_default(url == DEFAULT_COORD);
                ui.set_coord_url(view::without_scheme(&url).into());
                ui.set_coord_field(view::without_scheme(&url).into());
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

    /// ПЕРЕКЛЮЧЕНИЕ ВКЛАДОК ТУДА И ОБРАТНО НЕ УБИВАЕТ ОКНО.
    ///
    /// Регрессия на вылет v1.31: «Хост»/«Сервер» → «VPN» и приложение падало
    /// насмерть — `Recursion detected` в property-графе Slint. Виноват был
    /// `changed height` на карточке хоста: Slint вычисляет отслеживаемое
    /// выражение прямо при рождении карточки, а рождаются карточки внутри
    /// раскладки списка — та самая раскладка, из которой высота и берётся.
    ///
    /// ПУСТОЙ КАТАЛОГ ЭТОГО НЕ ЛОВИТ: нет строк — нет карточек — нет слежения.
    /// Поэтому хосты кладём обязательно, и обязательно ДО первой раскладки —
    /// иначе падения не будет, как его не было при пустом списке у владельца.
    ///
    /// «Раскладку» гоняем событием мыши: оно проходит по дереву и требует
    /// геометрию каждого элемента — ровно то, что делает отрисовка окна. Без
    /// такого пинка свойства раскладки ленивы и не вычисляются вовсе.
    #[test]
    fn switching_tabs_back_and_forth_keeps_the_window_alive() {
        use slint::LogicalPosition;
        use slint::platform::WindowEvent;

        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        ui.window().set_size(slint::LogicalSize::new(400.0, 820.0));
        ui.set_hosts(str_hosts(&["ALPHA", "BRAVO", "CHARLIE"]));
        // ОДИН ХОСТ ПОДКЛЮЧЁН — иначе свечение вокруг его карточки не родится
        // вовсе, и сторож стерёг бы разметку без него. Свечение читает ширину и
        // высоту карточки обычными привязками, но привязка и слежение — разные
        // вещи, и цена ошибки тут ровно та же: падение при переключении вкладок.
        ui.set_vpn_state(2);
        ui.set_vpn_host_id("BRAVO".into());
        ui.show().unwrap();

        // ВПН → Хост → ВПН → Сервер → ВПН, и ещё круг: владелец жаловался
        // ровно на этот порядок.
        for tab in [0, 1, 0, 2, 0, 1, 0, 2, 0] {
            ui.set_tab(tab);
            // Пинок раскладке: курсор посреди списка.
            ui.window()
                .dispatch_event(WindowEvent::PointerMoved { position: LogicalPosition::new(200.0, 400.0) });
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(50));
        }
        ui.hide().unwrap();
    }

    /// УМНАЯ ПРОКРУТКА ЖИВА: раскрытая карточка не остаётся под нав-баром.
    ///
    /// Часовой над починкой вылета: `changed height` заменён на часы, которые
    /// догоняют рост карточки. Если часы не пойдут (не тот признак в `running`,
    /// не тот вход в `reveal`), падение не случится — просто список замрёт, и
    /// «Подключить» у последней карточки уедет под бар, как было до умной
    /// прокрутки вовсе. Такое молчаливое возвращение и ловим.
    #[test]
    fn expanding_the_last_card_pulls_it_out_from_under_the_navbar() {
        use i_slint_backend_testing::ElementHandle;
        use slint::LogicalPosition;
        use slint::platform::WindowEvent;

        const WIN_H: f32 = 820.0;
        /// Сколько снизу закрыто нав-баром: 66 бар + 30 подъём + 18 завеса.
        const BAR_COVER: f32 = 114.0;

        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        ui.window().set_size(slint::LogicalSize::new(400.0, WIN_H));
        // Список ЗАВЕДОМО ДЛИННЕЕ ОКНА: на коротком прокручивать нечего, и тест
        // прошёл бы при любой поломке.
        ui.set_hosts(str_hosts(&["A", "B", "C", "D", "E", "F", "G", "H", "I", "J"]));
        ui.show().unwrap();
        ui.window().dispatch_event(WindowEvent::PointerMoved { position: LogicalPosition::new(200.0, 400.0) });

        // Мишень — ПОСЛЕДНЯЯ карточка, чья шапка ещё в видимой полосе: раскрыв
        // именно её, «Подключить» без умной прокрутки уедет под нав-бар.
        let cards: Vec<ElementHandle> = ElementHandle::find_by_element_id(&ui, "VpnPage::card").collect();
        let last = cards
            .iter()
            .rfind(|c| c.absolute_position().y + 20.0 < WIN_H - BAR_COVER)
            .expect("в видимой полосе нет ни одной карточки — тест меряет пустоту");

        // Тап по шапке.
        let head = LogicalPosition::new(200.0, last.absolute_position().y + 20.0);
        ui.window().dispatch_event(WindowEvent::PointerMoved { position: head });
        ui.window().dispatch_event(WindowEvent::PointerPressed { position: head, button: slint::platform::PointerEventButton::Left });
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(50));
        ui.window().dispatch_event(WindowEvent::PointerReleased { position: head, button: slint::platform::PointerEventButton::Left });

        // Карточка растёт 220мс, часы догоняют 8 тиков по 30мс. Даём с запасом.
        for _ in 0..20 {
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(30));
            ui.window().dispatch_event(WindowEvent::PointerMoved { position: LogicalPosition::new(200.0, 400.0) });
        }

        let top = last.absolute_position().y;
        let bottom = top + last.size().height;
        assert!(last.size().height > 200.0, "карточка не раскрылась: высота {}", last.size().height);
        assert!(
            bottom <= WIN_H - BAR_COVER + 1.0,
            "низ раскрытой карточки на {bottom} — под нав-баром (полоса кончается на {})",
            WIN_H - BAR_COVER,
        );
        assert!(top >= 0.0, "верх карточки уехал за край окна: {top}");
        ui.hide().unwrap();
    }

    /// Каталог из голых имён — только чтобы карточки были.
    #[cfg(test)]
    fn str_hosts(ids: &[&str]) -> ModelRc<HostRow> {
        ModelRc::new(VecModel::from(
            ids.iter()
                .map(|id| HostRow {
                    id: (*id).into(),
                    name: (*id).into(),
                    usable: true,
                    ping_alarm: -1,
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        ))
    }

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

    /// ОДНО И ТО ЖЕ ОКНО НА 100 %, 125 %, 150 % И 200 %.
    ///
    /// Жалоба «на Windows окно слишком широкое» первым делом ведёт сюда: у людей
    /// экран почти всегда масштабирован, а раннер CI идёт на 100 % и такой
    /// разницы не покажет. Проверка чисто арифметическая и потому гоняется на
    /// любой машине; за пиксели на живой Windows отвечает замер в
    /// tools/ci-shot-windows.ps1 (строки «SCALE …»).
    ///
    /// Утверждение сильнее, чем «ширина равна 400»: окно обязано занимать ОДНУ И
    /// ТУ ЖЕ ДОЛЮ ЭКРАНА при любом масштабе. Именно это и значит «выглядит
    /// одинаково»: в пикселях всё растёт вместе с масштабом, в точках — не
    /// меняется. Стоит перепутать точки с пикселями хоть в одном делении, и доля
    /// разъедется в полтора-два раза.
    ///
    /// Панели взяты настоящие: 125 % и 150 % живут на FullHD-ноутбуках, 200 % —
    /// на 4К. FullHD при 200 % сюда не попадает намеренно: там экран всего 540
    /// точек, и вступает нижний предел высоты (MIN_H) — сознательное нарушение
    /// доли, у него своя проверка выше.
    #[test]
    fn the_window_looks_the_same_at_every_screen_scale() {
        const TITLE_BAR: f32 = 40.0;

        for (pixels, scale) in [(1080.0_f32, 1.0_f32), (1080.0, 1.25), (1080.0, 1.5), (2160.0, 2.0)] {
            let screen = logical_screen_h(pixels, scale, true);
            assert_eq!(screen, pixels / scale, "масштаб {scale}: высота экрана не в точках");

            // Ширина в ТОЧКАХ от масштаба не зависит вовсе — она свойство макета.
            // В пикселях она растёт вместе с масштабом, и это правильно.
            assert_eq!(LAYOUT_W, 400.0, "масштаб {scale}: ширина макета уехала");

            // Доля экрана — та же самая. Считаем В ПИКСЕЛЯХ: экран физический,
            // окно логическое, и сравнить их можно только приведя к одному.
            let on_screen_px = (fit_height(screen) + TITLE_BAR) * scale;
            let share = on_screen_px / pixels;
            assert!(
                (share - 0.80).abs() < 0.01,
                "{pixels}px @{scale}: окно занимает {share} экрана вместо четырёх пятых \
                 ({on_screen_px} пикселей из {pixels})",
            );
        }
    }

    // Адрес координатора (показ без схемы и обратное приведение) проверяется
    // там, где теперь и живёт, — `bmv_common::view`. Здесь остаются проверки
    // самого окна.

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
                    (r.ping.to_string(), r.ping_alarm)
                })
                .collect();
            let _ = tx.send(seen);
        });

        let seen = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("host_row зависла — снова двойной lock в одном выражении");

        // В разметку уезжает УРОВЕНЬ ТРЕВОГИ, а не миллисекунды: пороги живут в
        // справочнике, окно их не повторяет. −1 — замера ещё не было.
        assert_eq!(seen[0], ("42 мс".to_string(), view::Alarm::Calm as i32));
        assert_eq!(seen[1], (view::PING_NO_ANSWER.to_string(), view::Alarm::Muted as i32));
        assert_eq!(seen[2], (String::new(), -1));
    }

    // Подпись протокола и часы сеанса переехали в `bmv_common::view` вместе со
    // своими проверками. Здесь стоял ещё и ручной пересказ терминальной ветки
    // (`bmv_cli_proto_agrees`) — сверка копии с копией. Копия теперь одна.

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

    /// КОГДА ПРОБОВАТЬ ЗАПАСНОЙ РИСОВАЛЬЩИК, А КОГДА НЕЛЬЗЯ.
    ///
    /// Три правила, и за каждым своя поломка:
    /// * окна не было ни разу → пробуем. Это и есть машина без видеокарты, и
    ///   решение НЕ зависит от того, как чужой код подписал свой отказ: сверка
    ///   по слову «OpenGL» пропускала половину отказов старта (winit не смог
    ///   поднять цикл событий, окно создалось, но не отдало handle, рисовальщик
    ///   не нашёлся вовсе);
    /// * окно человек уже видел → НИКОГДА. Иначе приложение воскресает после
    ///   того, как его закрыли, — прямой запрет;
    /// * метка запасного режима уже стоит → НИКОГДА, это петля перезапусков.
    ///
    /// Окружения решение не видит вовсе — и не должно: раньше предохранителем
    /// служила чужая `SLINT_BACKEND`, и пустая строка в ней выключала починку.
    #[test]
    fn the_fallback_is_tried_only_when_the_window_was_never_shown() {
        let args = |v: &[&str]| v.iter().map(std::ffi::OsString::from).collect::<Vec<_>>();

        // Обычный запуск, окна не было — единственный случай, когда пробуем.
        assert!(should_try_software(&args(&["bemyvpn-gui"]), false));
        // Чужие аргументы решению не мешают.
        assert!(should_try_software(&args(&["bemyvpn-gui", "--что-то"]), false));

        // Окно показывалось — не перезапускаемся ни при каких аргументах.
        assert!(!should_try_software(&args(&["bemyvpn-gui"]), true));
        assert!(!should_try_software(&args(&["bemyvpn-gui", "--что-то"]), true));

        // Метка запасного режима — второго перезапуска не будет.
        assert!(!should_try_software(&args(&["bemyvpn-gui", SW_FLAG]), false));
        assert!(!should_try_software(&args(&["bemyvpn-gui", SW_FLAG]), true));
    }

    /// ЗАПАСНОЙ РЕЖИМ НЕ ПЕРЕЖИВАЕТ ПЕРЕЗАПУСК, КОТОРЫЙ ДЕЛАЕМ НЕ МЫ.
    ///
    /// Обновление перезапускает приложение чужими руками:
    /// `bmv_common::update::spawn_exe_updater` пишет .cmd со `start "" "<exe>"`
    /// — БЕЗ аргументов, но с унаследованным ОКРУЖЕНИЕМ. Пока признак жил в
    /// `SLINT_BACKEND`, машина, один раз свалившаяся на растеризатор, оставалась
    /// на нём навсегда — в том числе после установки драйвера видеокарты.
    ///
    /// Отсюда правило: команда перезапуска не задаёт НИ ОДНОЙ переменной
    /// окружения, а метка едет аргументом.
    #[test]
    fn the_fallback_mode_does_not_survive_a_restart_someone_else_makes() {
        let cmd = software_restart(
            "/opt/bemyvpn/bemyvpn-gui".into(),
            [std::ffi::OsString::from("--свой-аргумент")].into_iter(),
        );

        assert_eq!(
            cmd.get_envs().count(),
            0,
            "запасной режим уехал в окружение — он переживёт чужой перезапуск",
        );

        // Метка — в аргументах, рядом с теми, что были у нас.
        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(args, ["--свой-аргумент", SW_FLAG]);

        // ПЕТЛЯ ЗАКРЫТА: потомок, увидев свои же аргументы, не перезапустится.
        let child: Vec<std::ffi::OsString> = std::iter::once(std::ffi::OsString::from("bemyvpn-gui"))
            .chain(cmd.get_args().map(|a| a.to_os_string()))
            .collect();
        assert!(software_mode(&child), "потомок не узнаёт свою же метку");
        assert!(!should_try_software(&child, false), "потомок перезапустится снова — это петля");
    }

    // ── НАСТОЯЩИЙ ПРОГРАММНЫЙ РАСТЕРИЗАТОР В ТЕСТЕ ──────────────────────────
    //
    // Тестовая оболочка Slint (`init_no_event_loop`) НЕ РИСУЕТ вовсе, а падение
    // без видеокарты живёт именно в рисовальщике. Поэтому здесь поднимается тот
    // же `SoftwareRenderer`, что и у `SLINT_BACKEND=winit-software`.
    //
    // Оболочка ПОВТОРЯЕТ winit в одной мелочи: `update_window_properties`
    // читает ограничения раскладки корня (winitwindowadapter.rs:1207). Slint
    // зовёт это при показе окна и потом по нулевому таймеру, когда свойства
    // окна меняются (window.rs:375), — так дерево «усаживается» ДО кадра. Без
    // этой мелочи оболочка краснела бы даже там, где живое окно живёт. А вот
    // таймеры МЕЖДУ правкой и кадром здесь не крутятся нарочно — почему,
    // сказано у `sw_frame`.
    const SW_W: u32 = 400;
    const SW_H: u32 = 820;

    struct SwWindow {
        window: slint::Window,
        renderer: slint::platform::software_renderer::SoftwareRenderer,
        size: std::cell::Cell<slint::PhysicalSize>,
    }

    impl slint::platform::WindowAdapter for SwWindow {
        fn window(&self) -> &slint::Window {
            &self.window
        }
        fn size(&self) -> slint::PhysicalSize {
            self.size.get()
        }
        fn set_size(&self, size: slint::WindowSize) {
            self.size.set(size.to_physical(1.));
            self.window
                .dispatch_event(slint::platform::WindowEvent::Resized { size: size.to_logical(1.) });
        }
        fn renderer(&self) -> &dyn slint::platform::Renderer {
            &self.renderer
        }
        fn update_window_properties(&self, properties: slint::platform::WindowProperties<'_>) {
            // ТО САМОЕ, ЧТО ДЕЛАЕТ WINIT. Читает `layout_info` корня, а вместе с
            // ним — всё, до чего раскладка корня дотягивается.
            let _ = properties.layout_constraints();
        }
    }

    thread_local! {
        static SW: RefCell<Option<Rc<SwWindow>>> = const { RefCell::new(None) };
    }

    struct SwPlatform;
    impl slint::platform::Platform for SwPlatform {
        fn create_window_adapter(
            &self,
        ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
            use slint::platform::software_renderer::{RepaintBufferType, SoftwareRenderer};
            let w = Rc::new_cyclic(|weak: &std::rc::Weak<SwWindow>| SwWindow {
                window: slint::Window::new(weak.clone()),
                // Тот же тип буфера, что softbuffer отдаёт на живой машине.
                renderer: SoftwareRenderer::new_with_repaint_buffer_type(
                    RepaintBufferType::ReusedBuffer,
                ),
                size: std::cell::Cell::new(slint::PhysicalSize::new(SW_W, SW_H)),
            });
            SW.with(|s| *s.borrow_mut() = Some(w.clone()));
            Ok(w)
        }
    }

    /// Окно на программном растеризаторе. Только ОДИН раз на поток теста:
    /// оболочка Slint ставится на поток и не сбрасывается.
    fn sw_window() -> AppWindow {
        slint::platform::set_platform(Box::new(SwPlatform)).unwrap();
        let ui = AppWindow::new().unwrap();
        ui.window().set_size(slint::LogicalSize::new(SW_W as f32, SW_H as f32));
        ui
    }

    /// Один кадр СРАЗУ ПОСЛЕ ПРАВКИ СВОЙСТВ — без прогона таймеров между ними.
    ///
    /// Это НЕ упрощение, а ХУДШИЙ ИЗ ЖИВЫХ ПОРЯДКОВ, и именно он убивал окно.
    /// Slint «усаживает» раскладку заранее в нулевом таймере (window.rs:375), а
    /// winit крутит таймеры в начале оборота цикла (event_loop.rs:470). Когда
    /// свойства меняются В ХОДЕ оборота (нажали кнопку, пришло обновление из
    /// ядра), перерисовка успевает раньше таймера — усадки не было, и дерево
    /// перестраивается уже под рисовальщиком. Прогони мы здесь таймеры, проверка
    /// зеленела бы на сломанном Slint.
    fn sw_frame() {
        let w = SW.with(|s| s.borrow().clone()).expect("окно не поднято");
        let mut buf = vec![
            slint::platform::software_renderer::Rgb565Pixel(0);
            (SW_W * SW_H) as usize
        ];
        w.renderer.render(&mut buf, SW_W as usize);
    }

    /// БЕЗ ВИДЕОКАРТЫ ОКНО НЕ УМИРАЕТ ОТ ТОГО, ЧТО С ЭКРАНА ЧТО-ТО ПРОПАЛО.
    ///
    /// Ловим то, из-за чего запасной путь был бесполезен: под
    /// `SLINT_BACKEND=winit-software` вкладка «Сервер» убивала ПРОЦЕСС при
    /// нажатии «Сохранить и проверить» — «RefCell already borrowed»,
    /// i-slint-core/partial_renderer.rs:818. Поломка чужая и она НАСТОЯЩАЯ, а не
    /// наша неаккуратность с моделями:
    ///
    ///   `compute_dirty_regions` (1.14.1, partial_renderer.rs:417) брал
    ///   `borrow_mut` кеша геометрии и НЕ ОТПУСКАЛ его, пока на строке 423 считал
    ///   габариты элемента. Расчёт габаритов — это раскладка, а раскладка лениво
    ///   достраивает `if`-блоки и повторители (`Conditional::ensure_updated`,
    ///   `Repeater::ensure_updated`). Достраивая, они СНОСЯТ прежние поддеревья,
    ///   а снос идёт в `free_graphics_resources` — за вторым `borrow_mut` того же
    ///   кеша. На видеокарте этого нет: femtovg не заворачивается в
    ///   `PartialRenderer` вовсе.
    ///
    /// Починено переходом на Slint 1.15 (upstream issue #9882, PR #9883:
    /// расчёт габаритов поднят ВЫШЕ взятия кеша). Нижняя граница закреплена в
    /// Cargo.toml — эта проверка сторожит её. Вернётся 1.14.x — покраснеет.
    ///
    /// ГОНЯЕМ ВСЕ ТРИ ВКЛАДКИ, а не только «Сервер»: гибли не списки, а любой
    /// элемент, исчезающий из кадра, — а таких `if`-ов на экранах десятки.
    #[test]
    fn the_software_renderer_survives_elements_vanishing_between_frames() {
        let ui = sw_window();
        ui.show().unwrap();
        sw_frame();

        // ── «Сервер»: то самое падение ──────────────────────────────────
        ui.set_tab(2);
        ui.set_server_history(str_model(&["a.example".to_string()]));
        sw_frame();
        // Условия ПАРЯЩЕЙ ПАНЕЛИ: `if coord-state == 1:` гасит разом четыре
        // блока (строка состояния, адрес, плитки, «ВАШ IP»). Ровно это и делает
        // «Сохранить и проверить»: связь сбрасывается в «проверяю».
        for s in [1, 2, 1, 0, 1, 2, 0] {
            ui.set_coord_state(s);
            sw_frame();
        }
        // Условие ВНУТРИ ПРОКРУТКИ + подпись про туннель.
        for (e, v) in [("беда", 2), ("", 0), ("беда", 2), ("", 0)] {
            ui.set_config_error(e.into());
            ui.set_coord_state(1);
            ui.set_vpn_state(v);
            sw_frame();
        }
        // Список недавних серверов: и рост, и УСЫХАНИЕ до пустого (тогда
        // пропадает и заголовок «Недавние серверы» — ещё два `if`).
        for h in [vec!["b.example"], vec!["c.example", "d.example"], vec![], vec!["e.example"]] {
            ui.set_server_history(str_model(&h.iter().map(|s| s.to_string()).collect::<Vec<_>>()));
            sw_frame();
        }

        // ── «VPN»: каталог, недавние, состояние туннеля ─────────────────
        ui.set_tab(0);
        sw_frame();
        for hosts in [vec!["ALPHA"], vec!["ALPHA", "BRAVO", "CHARLIE"], vec![], vec!["DELTA"]] {
            ui.set_hosts(str_hosts(&hosts));
            ui.set_recent_names(str_model(&hosts.iter().map(|s| s.to_string()).collect::<Vec<_>>()));
            ui.set_recent_ids(str_model(&hosts.iter().map(|s| s.to_string()).collect::<Vec<_>>()));
            sw_frame();
        }
        for s in [0, 1, 2, 3, 0] {
            ui.set_vpn_state(s);
            ui.set_vpn_host_id(if s == 2 { "DELTA".into() } else { "".into() });
            sw_frame();
        }
        // Блок обновления — целая карточка, появляющаяся и пропадающая.
        for v in ["1.99", "", "1.99", ""] {
            ui.set_update_version(v.into());
            sw_frame();
        }

        // ── «Хост»: раздача включается и выключается ────────────────────
        ui.set_tab(1);
        sw_frame();
        for s in [0, 1, 2, 3, 2, 0] {
            ui.set_host_state(s);
            ui.set_host_code(if s == 2 { "КОД123".into() } else { "".into() });
            ui.set_host_error(if s == 3 { "не вышло".into() } else { "".into() });
            sw_frame();
        }

        // Поля ввода: у них своя ветка рисования (`draw_text_input`), и на живом
        // экране набор с клавиатуры проверить не вышло — значит проверяем здесь.
        for t in ["", "к", "коротко", "очень длинное имя хоста для проверки", ""] {
            ui.set_host_name(t.into());
            ui.set_host_password(t.into());
            sw_frame();
            ui.set_tab(2);
            ui.set_coord_field(t.into());
            sw_frame();
            ui.set_tab(1);
        }

        // ── И ещё круг по вкладкам с живым каталогом ────────────────────
        for tab in [0, 2, 1, 0, 2, 1, 0] {
            ui.set_tab(tab);
            ui.set_hosts(str_hosts(if tab == 0 { &["ECHO", "FOXTROT"] } else { &[] }));
            sw_frame();
        }
        ui.hide().unwrap();
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
