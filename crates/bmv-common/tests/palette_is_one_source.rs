//! ЧАСОВОЙ ПРОТИВ ВТОРОЙ ПАЛИТРЫ: цвет живёт ровно в ОДНОМ месте.
//!
//! Источник — `design/palette.toml`; по четырём темам (Slint, Swift, Kotlin,
//! Rust) его раскладывает `design/sync-palette.py`. Часовой ловит ОБЕ беды:
//!
//!   1. ТЕМА РАЗОШЛАСЬ С ИСТОЧНИКОМ — кто-то поправил цвет прямо в теме, мимо
//!      источника. Ловится прогоном скрипта в режиме `--check`: он собирает
//!      участок заново и сравнивает побайтно.
//!   2. ОБОЛОЧКИ РАЗОШЛИСЬ МЕЖДУ СОБОЙ — ровно то, что уже случилось однажды:
//!      терминал два круга правок держал самый первый акцент `#5E93FF`, а окно
//!      кромку `#FFFFFF14` (0.0784) там, где на телефонах стояло 0.08. Ловится
//!      вторым тестом, независимо от скрипта: каждый цвет источника обязан
//!      лежать в КАЖДОЙ теме, в её собственной записи.
//!
//! ЖИТЬ ЭТОМУ ФАЙЛУ РЯДОМ С ОСТАЛЬНЫМИ ЧАСОВЫМИ, в `crates/bmv-common/tests/`
//! (там уже `one_place_per_rule.rs` и `no_code_by_substring.rs` — та же порода).
//! Здесь он лежит временно: `crates/` в этой работе был чужим участком. При
//! переносе поправить только `repo_root()` — на один `parent()` меньше.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Корень репозитория. `CARGO_MANIFEST_DIR` = `<корень>/apps/bmv-gui`.
fn repo_root() -> PathBuf {
    // crates/bmv-common → crates → корень репозитория.
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap().to_path_buf()
}

const BEGIN: &str = "── НАЧАЛО: значения из design/palette.toml";
const END: &str = "── КОНЕЦ: значения из design/palette.toml";

/// Размеченный участок темы — только то, что раскладывает скрипт.
fn marked(root: &Path, rel: &str) -> String {
    let src = std::fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
    let start = src.find(BEGIN).unwrap_or_else(|| panic!("{rel}: нет маркера начала участка"));
    let end = src.find(END).unwrap_or_else(|| panic!("{rel}: нет маркера конца участка"));
    assert!(end > start, "{rel}: маркеры участка перепутаны местами");
    src[start..end].to_string()
}

/// Цвета из `[colors]` источника: (имя, «08090B»). Разбор плоский — файл тоже
/// плоский, а полноценный TOML ради тридцати строк тянул бы зависимость.
fn palette_colors(root: &Path) -> Vec<(String, String)> {
    let src = std::fs::read_to_string(root.join("design/palette.toml")).expect("design/palette.toml");
    let mut out = Vec::new();
    let mut in_colors = false;
    for line in src.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_colors = line == "[colors]";
            continue;
        }
        if !in_colors || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, rest) = line.split_once('=').expect("строка вида «ключ = значение»");
        let hex = rest.trim().trim_start_matches('"').get(..7).expect("цвет вида «#RRGGBB»");
        out.push((key.trim().to_string(), hex.trim_start_matches('#').to_uppercase()));
    }
    assert!(out.len() >= 8, "в [colors] подозрительно мало цветов ({}) — сломан разбор", out.len());
    out
}

/// Беда №1: тема разошлась с источником.
///
/// Проверяет тем же кодом, что и раскладывает, — второго генератора нарочно нет:
/// два генератора и есть та самая вторая правда, от которой всё затевалось.
#[test]
fn every_theme_matches_the_palette_source() {
    let root = repo_root();
    let out = Command::new("python3")
        .arg("design/sync-palette.py")
        .arg("--check")
        .current_dir(&root)
        .output()
        // Молча пропустить проверку нельзя: часовой, который умеет не сработать,
        // не часовой.
        .expect("нужен python3 в PATH — им проверяется и раскладывается палитра");
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Беда №2: оболочки разошлись между собой.
///
/// Не зовёт скрипт вовсе: если участок кто-то сотрёт вместе с маркерами, первый
/// тест этого может и не заметить, а этот заметит.
#[test]
fn every_shell_carries_every_color() {
    let root = repo_root();
    let colors = palette_colors(&root);

    // Как цвет записан в каждой оболочке.
    let slint = marked(&root, "apps/bmv-gui/ui/theme.slint");
    let swift = marked(&root, "apps/ios/BeMyVPN/Theme.swift");
    let kotlin = marked(&root, "apps/android/app/src/main/java/org/bemyvpn/Theme.kt");
    let rust = marked(&root, "apps/bmv-cli/src/tui.rs");

    let mut missing = Vec::new();
    for (name, hex) in &colors {
        for (shell, text, literal) in [
            ("окно (Slint)", &slint, format!("#{hex}")),
            ("iPhone (Swift)", &swift, format!("0x{hex}")),
            ("Android (Kotlin)", &kotlin, format!("0xFF{hex}")),
        ] {
            if !text.contains(&literal) {
                missing.push(format!("{shell}: нет {name} = {literal}"));
            }
        }
    }

    // У терминала поверхностей нет, поэтому он берёт не все роли — только эти.
    // Список явный: «сколько-нибудь цветов» проверкой не является.
    for name in ["accent", "amber", "red", "dim", "fg", "bg", "card_hi"] {
        let hex = &colors.iter().find(|(n, _)| n == name).expect(name).1;
        let literal = format!(
            "Color::Rgb(0x{}, 0x{}, 0x{})",
            &hex[0..2],
            &hex[2..4],
            &hex[4..6]
        );
        if !rust.contains(&literal) {
            missing.push(format!("терминал (Rust): нет {name} = {literal}"));
        }
    }

    assert!(
        missing.is_empty(),
        "оболочки разошлись по цвету — а глазами это ловится только на снимках:\n  {}\n\
         Цвета живут в design/palette.toml; починить: python3 design/sync-palette.py",
        missing.join("\n  ")
    );
}

/// Беда №3: ЦВЕТ ПРИЕХАЛ В РАЗМЕТКУ САМ, МИМО ПАЛИТРЫ.
///
/// Первые два часовых сверяют ЧИСЛА в темах. Эта беда другая: числа в темах в
/// порядке, а на экран попадает цвет, которого в теме нет вовсе, — потому что
/// его рисует не наша разметка, а чужая. Так и случилось: ползунок «Лимит
/// гостей» был `Slider` из `std-widgets`, а тот красится приватным
/// `FluentPalette.accent-background` — системным синим `#005FB8`. Подменить его
/// нельзя (везде `out property`), в `design/palette.toml` синего нет вообще, и
/// ни один часовой не краснел: в разметке не было НИ ОДНОГО цветового литерала.
/// Замер со снимков: ход `#005FB8`, кольцо бегунка `#FFFFFF`, неактивная часть
/// `#040406` — и вдобавок на маке те же три места давали `#C7C7CC` и `#484849`,
/// потому что Slint подставляет стиль по системе сборки.
///
/// Отсюда три проверки — по одному способу протечки на оболочку.
#[test]
fn markup_does_not_bring_color_from_outside() {
    let root = repo_root();
    let mut missing = Vec::new();

    // ── 1. ОКНО: готовый виджет со своей темой ──────────────────────────────
    //
    // `ScrollView` разрешён нарочно и он в списке ЕДИНСТВЕННЫЙ: полосы прокрутки
    // у него выключены навсегда (`ScrollBarPolicy.always-off` в `PageScroll`),
    // то есть рисовать своим цветом ему нечего. Всякий другой виджет из набора
    // приезжает вместе с чужой палитрой — как приехал `Slider`.
    const STOCK_ALLOWED: &[&str] = &["ScrollView"];
    // ── 2. ОКНО: цвет числом на месте ───────────────────────────────────────
    //
    // Список ИСЧЕРПЫВАЮЩИЙ и намеренно короткий. Оба литерала — известный долг:
    // это «белое с альфой» ~0.09, написанное одинаково во всех трёх оболочках,
    // но безымянное. Своего ключа в источнике у него нет, а завести ключ значит
    // разложить его скриптом по четырём темам, включая терминал. Пока долг
    // записан здесь: добавить третий литерал молча теперь нельзя.
    const KNOWN_LITERALS: &[(&str, &str)] = &[
        ("apps/bmv-gui/ui/components.slint", "#FFFFFF0A"), // наведение на «Новый код»
        ("apps/bmv-gui/ui/vpn_page.slint", "#FFFFFF17"),   // дорожка полосы заполненности
    ];

    let ui = root.join("apps/bmv-gui/ui");
    let mut files: Vec<_> = std::fs::read_dir(&ui)
        .expect("apps/bmv-gui/ui")
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .filter(|n| n.ends_with(".slint") && n != "theme.slint")
        .collect();
    files.sort();
    assert!(files.len() >= 5, "в ui/ подозрительно мало разметки ({}) — сломан обход", files.len());

    let mut seen_literals = Vec::new();
    for name in &files {
        let rel = format!("apps/bmv-gui/ui/{name}");
        let src = std::fs::read_to_string(root.join(&rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
        // Комментарии — не разметка: в них цвета НАЗЫВАЮТ (вот этот часовой и
        // называет `#005FB8`), а не рисуют. Блочных комментариев в ui/ нет.
        for line in src.lines().map(|l| l.split("//").next().unwrap_or("")) {
            if line.contains("\"std-widgets.slint\"") {
                let names = line
                    .split('{')
                    .nth(1)
                    .and_then(|s| s.split('}').next())
                    .unwrap_or_default();
                for w in names.split(',').map(str::trim).filter(|w| !w.is_empty()) {
                    if !STOCK_ALLOWED.contains(&w) {
                        missing.push(format!(
                            "{rel}: `{w}` из std-widgets — это готовый виджет со своей темой. \
                             Собрать свой из Theme (образец — ValueSlider в components.slint)"
                        ));
                    }
                }
            }
            // Цветовой литерал: решётка и дальше только 16-ричные цифры (6 или 8).
            let bytes: Vec<char> = line.chars().collect();
            for (i, c) in bytes.iter().enumerate() {
                if *c != '#' {
                    continue;
                }
                let hex: String =
                    bytes[i + 1..].iter().take_while(|c| c.is_ascii_hexdigit()).collect();
                if hex.len() == 6 || hex.len() == 8 {
                    seen_literals.push((rel.clone(), format!("#{}", hex.to_uppercase())));
                }
            }
        }
    }
    for (rel, lit) in &seen_literals {
        if !KNOWN_LITERALS.iter().any(|(f, l)| f == rel && l == lit) {
            missing.push(format!(
                "{rel}: цвет {lit} написан числом на месте — он обязан приехать из Theme"
            ));
        }
    }
    for (rel, lit) in KNOWN_LITERALS {
        if !seen_literals.iter().any(|(f, l)| f == rel && l == lit) {
            missing.push(format!(
                "{rel}: литерала {lit} больше нет — это хорошая новость, \
                 вычеркните его из KNOWN_LITERALS, иначе список врёт"
            ));
        }
    }

    // ── 3. ТЕЛЕФОНЫ: система докрашивает то, о чём ей не сказали ────────────
    //
    // Здесь протечка ОБРАТНАЯ окну: не «чужой цвет написан», а «свой не написан
    // вовсе», и системе остаётся подставить свой акцент. Ловится единственным
    // способом — присутствием той самой одной подмены на корне: пропадёт она,
    // и синева вернётся молча, во все поля ввода разом.
    const GLOBALS: &[(&str, &[&str])] = &[
        (
            "apps/ios/BeMyVPN/BeMyVPNApp.swift",
            &[
                // Без него системный синий: тулбары листов, кнопка алерта,
                // каретка и «капли» выделения во всех полях ввода.
                ".tint(Theme.accent)",
                // `.tint` красит у Slider только пройденный ход; бегунок и
                // остаток дорожки настраиваются лишь через прокси UIKit.
                "UISlider.appearance().thumbTintColor",
                "UISlider.appearance().maximumTrackTintColor",
            ],
        ),
        (
            "apps/android/app/src/main/java/org/bemyvpn/ui/App.kt",
            &[
                "LocalTextSelectionColors provides", // иначе выделение — гугл-синий #4286F4
                "LocalRippleTheme provides",         // иначе рябь на бегунке чёрная
                "LocalContentColor provides",        // иначе запасной глиф чёрный по чёрному
            ],
        ),
    ];
    for (rel, needles) in GLOBALS {
        let src = std::fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
        for n in *needles {
            if !src.contains(n) {
                missing.push(format!(
                    "{rel}: пропала подмена `{n}` — цвет здесь снова назначает система"
                ));
            }
        }
    }

    assert!(
        missing.is_empty(),
        "в разметку приехал цвет мимо палитры:\n  {}\n\
         Цвета живут в design/palette.toml и приходят в разметку через Theme.",
        missing.join("\n  ")
    );
}

/// Беда №4: СИНЯЯ КРОМКА ПРОЛЕЗЛА ВНУТРЬ БЛОКА.
///
/// Кромка в приложении синяя (`outline` — второе кольцо иконки), но НЕ ВЕЗДЕ:
/// синий она только там, где элемент лежит ПРЯМО НА СТРАНИЦЕ и кромка держит
/// границу. Внутри блока — на плитке парящей панели, на плашке флага в карточке,
/// на негромкой кнопке — кромка остаётся БЕЛОЙ (`hairline-inner`): там перепад
/// заливки и так 8.21 по L*, кромке нечего держать, и синий читался бы как
/// подсветка, то есть как «это выделено», чего у плитки нет.
///
/// ПРАВИЛО, КОТОРОЕ ДЕРЖИТСЯ ПАМЯТЬЮ, НЕ ДЕРЖИТСЯ. Первые три часовых сверяют
/// ЧИСЛА в темах — а эта беда числа не трогает: и синий, и белый в теме на
/// месте, просто в разметке взяли не тот. Отличить их можно ровно одним
/// способом — знать, где белому место, и не пускать его больше никуда.
///
/// СПИСОК ИСЧЕРПЫВАЮЩИЙ И СВЕРЯЕТСЯ В ОБЕ СТОРОНЫ: пропала строка — кто-то
/// покрасил внутренность синим; появилась лишняя — кто-то увёл страницу в белый.
/// Оттого он и не рассыпается со временем: список, который врёт, краснеет сам.
///
/// ЧЕГО ЭТОТ ЧАСОВОЙ НЕ ЛОВИТ: плитку, собранную В ОБХОД общей заготовки
/// (`TileFrame` в окне, `tileBackground` на телефонах). Такую он увидит только
/// как лишнее место в списке, если её сделают белой, — а синюю не увидит вовсе.
/// Это осознанный потолок: чтобы поймать её, надо разбирать вложенность разметки
/// на трёх языках, а плитки мимо заготовки в приложении нет ни одной.
#[test]
fn the_blue_edge_does_not_creep_inside_blocks() {
    // Где кромке положено быть БЕЛОЙ, и почему это «внутри блока».
    const INNER: &[(&str, &str)] = &[
        // ── окно ────────────────────────────────────────────────────────────
        // Заготовка всех плиток: и на панели состояния, и в раскрытой карточке.
        ("apps/bmv-gui/ui/components.slint", "in property <color> stroke: Theme.hairline-inner;"),
        // Плитка, которая копируется тапом (код, IP) — та же заготовка.
        ("apps/bmv-gui/ui/components.slint", "copied ? Theme.edge-done(Theme.accent) : Theme.hairline-inner;"),
        // «Новый код» — негромкая кнопка ВНУТРИ парящей панели.
        ("apps/bmv-gui/ui/components.slint", "root.did ? Theme.edge-done(Theme.accent) : Theme.hairline-inner;"),
        // Плашка флага 56×56 — внутри карточки хоста, а не на странице.
        ("apps/bmv-gui/ui/vpn_page.slint", "border-color: Theme.hairline-inner;"),
        // Пароль гостя — единственное поле ввода НЕ на странице: оно живёт в
        // раскрытой карточке, рядом с плитками, и берёт их кромку. Остальные
        // четыре поля лежат на странице и синие (`Field` без `stroke`).
        ("apps/bmv-gui/ui/vpn_page.slint", "stroke: Theme.hairline-inner;"),
        // ── iPhone ──────────────────────────────────────────────────────────
        ("apps/ios/BeMyVPN/Kit.swift", "accent ?? Theme.hairlineInner"), // заготовка плиток
        ("apps/ios/BeMyVPN/Kit.swift", "Theme.edgeDone() : Theme.hairlineInner"), // «Новый код»
        // Плашка флага 56×56 — та же, что в окне: внутри карточки хоста.
        ("apps/ios/BeMyVPN/ContentView.swift", "cornerRadius: 14, style: .continuous).stroke(Theme.hairlineInner"),
        // Пароль гостя в раскрытой карточке — он же.
        ("apps/ios/BeMyVPN/ContentView.swift", "cornerRadius: 11, style: .continuous).stroke(Theme.hairlineInner"),
        // ── Android ─────────────────────────────────────────────────────────
        ("apps/android/app/src/main/java/org/bemyvpn/ui/Common.kt", "accent ?: Theme.hairlineInner"),
        ("apps/android/app/src/main/java/org/bemyvpn/ui/Common.kt", "Theme.edgeDone() else Theme.hairlineInner"),
        // Плашка флага 56×56 — она же на Android.
        ("apps/android/app/src/main/java/org/bemyvpn/ui/VpnTab.kt", "Theme.hairlineInner, RoundedCornerShape(14.dp)"),
        // Пароль гостя в раскрытой карточке — он же.
        ("apps/android/app/src/main/java/org/bemyvpn/ui/VpnTab.kt", "Theme.hairlineInner, RoundedCornerShape(11.dp)"),
    ];

    let root = repo_root();
    // Как белая кромка зовётся в каждой оболочке. Терминал не спрашиваем: рамок
    // он не рисует вовсе — см. `[colors] outline` в источнике.
    let tokens = ["Theme.hairline-inner", "Theme.hairlineInner"];

    let mut missing = Vec::new();
    for (rel, needle) in INNER {
        let src = std::fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
        if !src.contains(needle) {
            missing.push(format!(
                "{rel}: пропало «{needle}» — здесь кромка обязана быть БЕЛОЙ. \
                 Внутри блока синяя читается как подсветка, а не как граница"
            ));
        }
    }

    // Обратная сторона: белая кромка не должна появиться НИГДЕ, кроме списка.
    // Считаем по всем оболочкам сразу — тогда новое место видно, в какой бы из
    // них его ни завели.
    let mut seen = Vec::new();
    for rel in shell_markup(&root) {
        let src = std::fs::read_to_string(root.join(&rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
        for line in src.lines() {
            // Комментарии — не разметка: в них кромку НАЗЫВАЮТ, а не рисуют.
            let code = line.split("//").next().unwrap_or("");
            for t in tokens {
                if code.contains(t) {
                    seen.push(format!("{rel}: {}", line.trim()));
                }
            }
        }
    }
    if seen.len() != INNER.len() {
        missing.push(format!(
            "белая кромка стоит в {} местах, а в списке их {}. \
             Каждое место белой кромки обязано быть в INNER — иначе список врёт:\n    {}",
            seen.len(),
            INNER.len(),
            seen.join("\n    ")
        ));
    }

    assert!(
        missing.is_empty(),
        "правило «кромка синяя, но НЕ ВЕЗДЕ» сломалось:\n  {}\n\
         Синяя (`hairline`, `hairline-float`) — то, что лежит на СТРАНИЦЕ и сам парящий слой.\n\
         Белая (`hairline-inner`) — то, что лежит ВНУТРИ блока. Разбор — в theme.slint.",
        missing.join("\n  ")
    );
}

/// Разметка всех графических оболочек — там, где кромку РИСУЮТ. Темы сюда не
/// входят: в них кромку объявляют, и белый токен обязан там быть по построению.
fn shell_markup(root: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(root.join("apps/bmv-gui/ui"))
        .expect("apps/bmv-gui/ui")
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .filter(|n| n.ends_with(".slint") && n != "theme.slint")
        .map(|n| format!("apps/bmv-gui/ui/{n}"))
        .collect();
    for (dir, ext, theme) in [
        ("apps/ios/BeMyVPN", ".swift", "Theme.swift"),
        ("apps/android/app/src/main/java/org/bemyvpn/ui", ".kt", ""),
    ] {
        let mut more: Vec<String> = std::fs::read_dir(root.join(dir))
            .unwrap_or_else(|e| panic!("{dir}: {e}"))
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .filter(|n| n.ends_with(ext) && n != theme)
            .map(|n| format!("{dir}/{n}"))
            .collect();
        out.append(&mut more);
    }
    out.sort();
    assert!(out.len() >= 15, "разметки подозрительно мало ({}) — сломан обход", out.len());
    out
}

/// Канон бренда лежит не только в SVG.
///
/// Логотип «Звено» палитре НЕ подчиняется (см. brand/README.md — «не
/// перекрашивать половинки»), поэтому его цвета не раскладываются по темам. Но
/// сырыми они лежать не должны: `[brand]` в источнике — их именованное место, а
/// этот тест держит источник и картинки в одной правде. Однажды в окне уже жила
/// третья версия логотипа с цветами позапрошлой палитры.
#[test]
fn brand_rings_match_the_canon() {
    let root = repo_root();
    let src = std::fs::read_to_string(root.join("design/palette.toml")).expect("design/palette.toml");
    let logo = std::fs::read_to_string(root.join("brand/logo.svg")).expect("brand/logo.svg");
    let icon = std::fs::read_to_string(
        root.join("apps/android/app/src/main/res/drawable/ic_launcher_foreground.xml"),
    )
    .expect("android adaptive icon");

    let mut in_brand = false;
    let mut checked = 0;
    for line in src.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_brand = line == "[brand]";
            continue;
        }
        if !in_brand || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, rest) = line.split_once('=').expect("строка вида «ключ = значение»");
        let hex = rest.trim().trim_start_matches('"')[1..7].to_uppercase();
        let (key, logo_up, icon_up) = (key.trim(), logo.to_uppercase(), icon.to_uppercase());
        assert!(logo_up.contains(&hex), "brand/logo.svg разошёлся с [brand]: нет {key} = #{hex}");
        assert!(
            icon_up.contains(&hex),
            "андроидная иконка разошлась с [brand]: нет {key} = #{hex}"
        );
        checked += 1;
    }
    assert_eq!(checked, 4, "в [brand] должно быть четыре цвета колец, найдено {checked}");
}
