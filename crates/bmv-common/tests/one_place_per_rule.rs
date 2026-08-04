//! ЧАСОВОЙ ПРОТИВ РАСХОЖДЕНИЯ: правило показа живёт ровно в ОДНОМ месте.
//!
//! До этого справочника (`bmv_common::view`) каждое правило существовало копиями
//! в четырёх оболочках, и копии разошлись МОЛЧА: пустой протокол в окне —
//! «Обычный», на телефоне — «Без шифра»; часы сеанса в терминале — «12 мин»;
//! забитый хост в терминале давал нажать «Подключить». Ни одна проверка при этом
//! не краснела, потому что каждая копия по отдельности компилировалась и даже
//! была права по-своему.
//!
//! Здесь два часовых:
//!   1. `a_display_rule_exists_in_exactly_one_place` — грепалка по боевому Rust:
//!      признак правила за пределами `view.rs` = вторая правда, тест падает.
//!   2. `ping_thresholds_match_the_window_skin` — та же линейка пинга в разметке
//!      окна (`ui/*.slint`): её оттуда не позвать, значит сверяем числами.
//!
//! Приём одолжен у `bmv-config` (`every_knob_is_read_by_someone`): дешевле
//! поймать грепом, чем каждый раз ловить руками.

use std::path::{Path, PathBuf};

use bmv_common::view::{PING_AMBER_MS, PING_RED_MS};

/// Где живёт боевой код. `vendor/` — чужой, `target/` — сборка.
const ROOTS: [&str; 3] = ["crates", "apps", "server"];

/// Сам справочник — единственное законное место для всех признаков ниже.
const HOME: &str = "crates/bmv-common/src/view.rs";

/// Правило → признаки, которые обязаны встретиться НА ОДНОЙ строке → где ещё
/// законно (кроме `HOME`).
///
/// Признак выбран так, чтобы его нельзя было не написать, переписывая правило
/// заново: подпись, из которой правило состоит, или сравнение, в котором вся его
/// суть. Признаки пишем В НИЖНЕМ РЕГИСТРЕ — сравнение регистр не различает,
/// потому что копия в терминале разошлась с тремя другими оболочками ровно на
/// нём («нет связи» против «Нет связи»), и грепалка с учётом регистра такую
/// копию не видела.
type Rule = (&'static str, &'static [&'static str], &'static [&'static str]);
const RULES: &[Rule] = &[
    // Подпись протокола: заново её пишут только вместе с этим словом. Без
    // кавычек — оболочка вольна приклеить к подписи свой значок («🎭 Маскировка»).
    (
        "имя протокола (view::proto_name)",
        &["маскировка"],
        // Терминал принимает это слово КАК ВВОД («--protocol маскировка») и
        // переводит его в идентификатор протокола. Направление обратное показу:
        // человек → машина, а не машина → экран. Из справочника такое не берут.
        &["apps/bmv-cli/src/main.rs"],
    ),
    ("часы сеанса (view::session_clock)", &["% 3600"], &[]),
    ("подпись пинга (view::ping)", &["мс\""], &[]),
    ("годен ли хост (view::host_usable)", &[".guests <"], &[]),
    (
        "адрес координатора (view::coordinator_url)",
        &["\"http://\""],
        // bmv-signal переводит адрес координатора в адрес ВЕБСОКЕТА
        // (https→wss, http→ws). Это транспорт, а не разбор того, что набрал
        // человек: показывать там нечего и подставлять схему не надо.
        &["crates/bmv-signal/src/lib.rs"],
    ),
    ("имя хоста (view::host_display_name)", &["name.is_empty()", "id"], &[]),
    // ── связь с координатором и состояние VPN ──
    // Три состояния связи лежали ЧЕТЫРЬМЯ копиями, и четвёртая уже разошлась.
    ("состояние связи (view::link_state)", &["на связи"], &[]),
    ("состояние связи (view::link_state)", &["проверяю связ"], &[]),
    (
        "состояние связи (view::link_state)",
        &["восстанавлива"],
        // bmv-signal сообщает об ОШИБКЕ операции («сокет не поднят»), а не
        // рисует строку состояния: это ответ на конкретный запрос, а не то, что
        // висит на экране постоянно.
        &["crates/bmv-signal/src/lib.rs"],
    ),
    ("пустой каталог (view::empty_directory_hint)", &["хостов пока нет"], &[]),
    ("состояние VPN (view::vpn_text)", &["vpn выключен"], &[]),
    ("состояние VPN (view::vpn_text)", &["подключаюсь"], &[]),
    ("состояние VPN (view::vpn_text)", &["переподключение"], &[]),
    ("состояние VPN (view::vpn_text)", &["завершил раздачу"], &[]),
    ("состояние VPN (view::vpn_text)", &["связь с хостом пропала"], &[]),
];

/// ПЕРЕЕЗД В РАБОТЕ — копии, которые ещё не позвали справочник.
///
/// Правило въехало в `view.rs` целиком и с тестами; оболочки на него переводит
/// человек, потому что три из четырёх написаны не на Rust. Пока копия жива, она
/// перечислена здесь — и тест ТРЕБУЕТ, чтобы она и правда была на месте: убрали
/// копию, но забыли вычеркнуть строку → тест падает и напоминает. Так список не
/// превращается в вечное разрешение писать правило вторым местом.
///
/// ПУСТОЙ СПИСОК — ЦЕЛЬ. Пополнять его новыми строками нельзя: новая копия — это
/// то самое расхождение, ради поимки которого здесь всё и стоит.
const MIGRATING: &[(&str, &str)] = &[
    // Терминал: своя тройка состояний связи, разошедшаяся с тремя другими
    // оболочками регистром и многоточием, и свой набор подписей VPN.
    ("apps/bmv-cli/src/tui.rs", "на связи"),
    ("apps/bmv-cli/src/tui.rs", "восстанавлива"),
    ("apps/bmv-cli/src/tui.rs", "vpn выключен"),
    ("apps/bmv-cli/src/tui.rs", "подключаюсь"),
    ("apps/bmv-cli/src/tui.rs", "завершил раздачу"),
    ("apps/bmv-cli/src/main.rs", "завершил раздачу"),
    // Окно: подписи состояния VPN раздаются из Rust в разметку строками.
    ("apps/bmv-gui/src/main.rs", "vpn выключен"),
    ("apps/bmv-gui/src/main.rs", "подключаюсь"),
    ("apps/bmv-gui/src/main.rs", "завершил раздачу"),
    // ── НЕ-RUST ОБОЛОЧКИ ──────────────────────────────────────────────────
    // Телефоны и разметка окна живут на СОБСТВЕННЫХ копиях всего справочника:
    // позвать `view.rs` оттуда сегодня нечем, дверь открывает мост `bmv-ffi`
    // (`bmv_proto_name`, `bmv_ping_text`, `bmv_session_clock`, …). Пока копия
    // жива — она названа здесь поимённо; переедет — строка обязана уйти, иначе
    // часовой напомнит сам. Пополнять этот кусок новыми строками НЕЛЬЗЯ: новая
    // копия и есть то расхождение, ради поимки которого всё это стоит.
    ("apps/android/app/src/main/java/org/bemyvpn/AppState.kt", "восстанавлива"),
    ("apps/android/app/src/main/java/org/bemyvpn/AppState.kt", "завершил раздачу"),
    ("apps/android/app/src/main/java/org/bemyvpn/AppState.kt", "мс\""),
    ("apps/android/app/src/main/java/org/bemyvpn/AppState.kt", "подключаюсь"),
    ("apps/android/app/src/main/java/org/bemyvpn/AppState.kt", "связь с хостом пропала"),
    ("apps/android/app/src/main/java/org/bemyvpn/BmvVpnService.kt", "подключаюсь"),
    ("apps/android/app/src/main/java/org/bemyvpn/HostService.kt", "восстанавлива"),
    ("apps/android/app/src/main/java/org/bemyvpn/Model.kt", "% 3600"),
    ("apps/android/app/src/main/java/org/bemyvpn/Model.kt", "маскировка"),
    ("apps/android/app/src/main/java/org/bemyvpn/Native.kt", "завершил раздачу"),
    ("apps/android/app/src/main/java/org/bemyvpn/Native.kt", "маскировка"),
    ("apps/android/app/src/main/java/org/bemyvpn/Native.kt", "переподключение"),
    ("apps/android/app/src/main/java/org/bemyvpn/Native.kt", "подключаюсь"),
    ("apps/android/app/src/main/java/org/bemyvpn/ui/Common.kt", "маскировка"),
    ("apps/android/app/src/main/java/org/bemyvpn/ui/ServerTab.kt", "\"http://\""),
    ("apps/android/app/src/main/java/org/bemyvpn/ui/ServerTab.kt", "восстанавлива"),
    ("apps/android/app/src/main/java/org/bemyvpn/ui/ServerTab.kt", "мс\""),
    ("apps/android/app/src/main/java/org/bemyvpn/ui/ServerTab.kt", "на связи"),
    ("apps/android/app/src/main/java/org/bemyvpn/ui/ServerTab.kt", "проверяю связ"),
    ("apps/android/app/src/main/java/org/bemyvpn/ui/VpnTab.kt", "vpn выключен"),
    ("apps/android/app/src/main/java/org/bemyvpn/ui/VpnTab.kt", "восстанавлива"),
    ("apps/android/app/src/main/java/org/bemyvpn/ui/VpnTab.kt", "переподключение"),
    ("apps/android/app/src/main/java/org/bemyvpn/ui/VpnTab.kt", "подключаюсь"),
    ("apps/android/app/src/main/java/org/bemyvpn/ui/VpnTab.kt", "хостов пока нет"),
    ("apps/bmv-gui/ui/app.slint", "vpn выключен"),
    ("apps/bmv-gui/ui/host_page.slint", "маскировка"),
    ("apps/bmv-gui/ui/server_page.slint", "восстанавлива"),
    ("apps/bmv-gui/ui/server_page.slint", "мс\""),
    ("apps/bmv-gui/ui/server_page.slint", "на связи"),
    ("apps/bmv-gui/ui/server_page.slint", "проверяю связ"),
    ("apps/bmv-gui/ui/vpn_page.slint", "восстанавлива"),
    ("apps/bmv-gui/ui/vpn_page.slint", "хостов пока нет"),
    ("apps/ios/BeMyVPN/BeMyVPNApp.swift", "% 3600"),
    ("apps/ios/BeMyVPN/BeMyVPNApp.swift", "завершил раздачу"),
    ("apps/ios/BeMyVPN/BeMyVPNApp.swift", "мс\""),
    ("apps/ios/BeMyVPN/BeMyVPNApp.swift", "подключаюсь"),
    ("apps/ios/BeMyVPN/ContentView.swift", "\"http://\""),
    ("apps/ios/BeMyVPN/ContentView.swift", "vpn выключен"),
    ("apps/ios/BeMyVPN/ContentView.swift", "восстанавлива"),
    ("apps/ios/BeMyVPN/ContentView.swift", "маскировка"),
    ("apps/ios/BeMyVPN/ContentView.swift", "мс\""),
    ("apps/ios/BeMyVPN/ContentView.swift", "на связи"),
    ("apps/ios/BeMyVPN/ContentView.swift", "переподключение"),
    ("apps/ios/BeMyVPN/ContentView.swift", "подключаюсь"),
    ("apps/ios/BeMyVPN/ContentView.swift", "проверяю связ"),
    ("apps/ios/BeMyVPN/ContentView.swift", "хостов пока нет"),
    ("apps/ios/BeMyVPNTunnel/PacketTunnelProvider.swift", "завершил раздачу"),
    ("apps/ios/BeMyVPNTunnel/PacketTunnelProvider.swift", "связь с хостом пропала"),
];

/// ГРЕПАЛКА ВИДИТ ВСЕ ЧЕТЫРЕ ОБОЛОЧКИ.
///
/// Здесь стояло только `rs` с доводом: «включать расширения бессмысленно, пока
/// копию нечем заменить — тест стал бы вечно красным списком того, что
/// запрещено чинить». Довод верный, а вывод из него — нет: ровно для этого и
/// заведён `MIGRATING`. Со списком известных копий часовой РАБОТАЕТ уже
/// сегодня — он пропускает поимённо перечисленный долг и ловит НОВУЮ копию, —
/// а долг сам себя вычёркивает: убрал копию, не убрал строку → тест напомнит.
///
/// Пока расширений не было, три оболочки из четырёх были для часового
/// невидимы: любую новую копию правила в Kotlin, Swift или разметке окна можно
/// было завести молча — то есть именно там, где правила уже расходились.
const EXTS: [&str; 4] = ["rs", "kt", "swift", "slint"];

fn source_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name();
        let name = name.to_string_lossy();
        if p.is_dir() {
            // Каталоги `tests` мимо: в тесте оболочки повторить подпись законно —
            // тест как раз и проверяет, что оболочка показывает ровно её.
            if !matches!(&*name, "target" | "tests" | "build" | ".git" | "vendor" | "node_modules") {
                source_files(&p, out);
            }
        } else if p.extension().is_some_and(|x| EXTS.iter().any(|e| *e == x)) {
            out.push(p);
        }
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap().to_path_buf()
}

#[test]
fn a_display_rule_exists_in_exactly_one_place() {
    let root = repo_root();
    let mut files = Vec::new();
    for r in ROOTS {
        source_files(&root.join(r), &mut files);
    }
    assert!(files.len() > 20, "грепалка ничего не нашла — сломан обход каталогов ({})", root.display());

    let home = root.join(HOME);
    assert!(home.exists(), "справочник правил переехал — поправь HOME в этом тесте");

    let mut guilty = Vec::new();
    // Строки переезда, которые НЕ НАШЛИСЬ: копию убрали, а запись осталась.
    let mut stale: Vec<&(&str, &str)> = MIGRATING.iter().collect();
    for f in &files {
        if *f == home {
            continue; // дом правила
        }
        let rel = f.strip_prefix(&root).unwrap_or(f).display().to_string().replace('\\', "/");
        let Ok(src) = std::fs::read_to_string(f) else { continue };
        // Всё после `#[cfg(test)]` — тесты: там повтор подписи законен.
        let code = src.split("#[cfg(test)]").next().unwrap_or("");
        for (i, line) in code.lines().enumerate() {
            let t = line.trim_start().to_lowercase();
            if t.starts_with("//") {
                continue; // комментарии как раз ОБЪЯСНЯЮТ правило, а не повторяют
            }
            for (rule, needles, allowed) in RULES {
                if allowed.contains(&&*rel) {
                    continue;
                }
                if !needles.iter().all(|n| t.contains(n)) {
                    continue;
                }
                // Копия, которую владелец переводит руками: отмечаем найденной и
                // не виним. Всё, чего нет в списке, — виновно.
                if let Some(k) = stale.iter().position(|(p, n)| *p == rel && needles.contains(n)) {
                    stale.swap_remove(k);
                    continue;
                }
                if MIGRATING.iter().any(|(p, n)| *p == rel && needles.contains(n)) {
                    continue; // та же копия на второй строке того же файла
                }
                guilty.push(format!("{rel}:{}: {rule}\n      {}", i + 1, line.trim_start()));
            }
        }
    }

    assert!(
        guilty.is_empty(),
        "правило показа заведено вторым местом — рано или поздно эти двое разойдутся:\n  {}\n\
         Правило живёт в {HOME} целиком; оболочка его ЗОВЁТ, а не повторяет.",
        guilty.join("\n  ")
    );
    assert!(
        stale.is_empty(),
        "переезд состоялся, а список не убран — эти строки MIGRATING больше ничего не прикрывают:\n  {}\n\
         Вычеркни их: пока они здесь, на этом месте можно молча завести копию заново.",
        stale.iter().map(|(p, n)| format!("{p} — «{n}»")).collect::<Vec<_>>().join("\n  ")
    );
}

/// Пороги пинга в разметке окна — те же числа, что в справочнике.
///
/// Slint не умеет звать Rust-функции из выражения цвета, поэтому линейка там
/// записана числами. Значит либо сверять, либо однажды обнаружить, что окно
/// краснеет с 400 мс, а всё остальное — с 500.
#[test]
fn ping_thresholds_match_the_window_skin() {
    let root = repo_root();
    // Оба места, где окно раскрашивает пинг: плитка хоста и строка координатора.
    for rel in ["apps/bmv-gui/ui/components.slint", "apps/bmv-gui/ui/server_page.slint"] {
        let src = std::fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
        for needle in [format!("< {PING_AMBER_MS}"), format!("<= {PING_RED_MS}")] {
            assert!(
                src.contains(&needle),
                "{rel}: линейка пинга разошлась со справочником — там нет «{needle}»\n\
                 (bmv_common::view: янтарь с {PING_AMBER_MS} мс, красный после {PING_RED_MS} мс)"
            );
        }
    }
}
