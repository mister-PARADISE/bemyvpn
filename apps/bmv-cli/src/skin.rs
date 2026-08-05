//! НАБОР ДЕТАЛЕЙ ТЕРМИНАЛЬНОЙ ОБОЛОЧКИ — оформление в одном месте.
//!
//! ЧТО ЭТО. То же, чем для окна служит `apps/bmv-gui/ui/components.slint`, а для
//! Android — `ui/Common.kt`: готовые куски вида, из которых собран экран. У
//! терминала такого места не было — рамка, значок состояния, строка настройки и
//! тост писались руками прямо там, где рисуются, и уже разошлись между собой:
//! одна и та же строка «подпись — значение» выравнивалась по 14 знакам на
//! вкладке «Хост» и по 16 на «Сервере», а значок состояния был выписан двадцатью
//! отдельными строками, в которых «нет связи» успело стать то янтарным, то
//! красным.
//!
//! ДОГОВОР: У ДЕТАЛИ НЕТ ПАРАМЕТРА ЦВЕТА. Наружу отсюда не отдаётся ни одного
//! `Color` — только готовые `Span`/`Style`/`Block`. Параметры у деталей по
//! смыслу: состояние (`State`), уровень тревоги (`view::Alarm`), годен ли хост.
//! Иначе через месяц в коде снова будет пять почти одинаковых оттенков, только
//! уже через функцию.
//!
//! ГДЕ ЦВЕТА. В `tui.rs`, между маркерами, которые пишет
//! `design/sync-palette.py` из `design/palette.toml`. Сюда они не переехали
//! нарочно — см. объяснение у объявления модуля в `tui.rs`.
//!
//! ЧЕГО ЗДЕСЬ НЕТ. Подписей. Слова про состояние связи, VPN, пинг и пустой
//! каталог живут в общем справочнике (`bmv_common::view`) — деталь получает
//! готовый текст и только одевает его.

use bmv_common::view::Alarm;
use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, ListItem, Padding};

use super::{ACCENT, AMBER, BG, DIM, FG, RED, SEL};

// ── Состояние ────────────────────────────────────────────────────────────────

/// Что показывает значок состояния — СМЫСЛ, а не картинка и не цвет.
///
/// Четыре смысла на всё приложение: связь с координатором, VPN, раздача, свой
/// сервер, строка каталога. Раньше каждое из двадцати мест выбирало значок и
/// цвет само, и выбор разъезжался: обрыв связи в шапке краснел, а в карточке
/// «Связь» желтел — про одно и то же состояние два разных ответа на одном
/// экране.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    /// Работает.
    On,
    /// Идёт / ещё не готово — само дойдёт.
    Wait,
    /// Выключено, или ещё не знаем.
    Off,
    /// Сломалось — само не пройдёт.
    Err,
}

/// Уровень тревоги справочника → состояние значка.
///
/// Отдельного правила «когда связь красная» у терминала быть не должно: это
/// решает `view::link_state`, а оболочка только одевает. `Muted` («ещё не
/// знаем») — это `Off`, а не `Wait`: ждать нечего, мы просто пока не спросили.
impl From<Alarm> for State {
    fn from(a: Alarm) -> Self {
        match a {
            Alarm::Calm => State::On,
            Alarm::Amber => State::Wait,
            Alarm::Red => State::Err,
            Alarm::Muted => State::Off,
        }
    }
}

/// Значок состояния.
///
/// ВСЕ ЧЕТЫРЕ — ЭМОДЗИ-ПРЕЗЕНТАЦИИ ОДНОЙ ШИРИНЫ (2). У «выключено» здесь стоял
/// голый U+26AA без селектора: терминал считает его шириной 1, и строка с ним
/// ехала на знак левее соседних. Ширину стережёт тест внизу файла.
pub fn state_dot(s: State) -> &'static str {
    match s {
        State::On => "🟢",
        State::Wait => "🟡",
        State::Off => "⚪️",
        State::Err => "🔴",
    }
}

/// Как выглядит состояние. Наружу не выдаётся — иначе это и есть «попроси любой
/// цвет», от которого весь этот файл.
fn state_style(s: State) -> Style {
    match s {
        State::On => Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        State::Wait => Style::default().fg(AMBER),
        State::Off => Style::default().fg(DIM),
        State::Err => Style::default().fg(RED),
    }
}

/// Строка состояния: значок и подпись одним куском, одетые по смыслу.
///
/// Подпись приходит готовой (из `view::vpn_text`, `view::link_state` или текста
/// отказа) — своих слов деталь не сочиняет.
pub fn state_line(s: State, text: impl std::fmt::Display) -> Span<'static> {
    Span::styled(format!("{} {text}", state_dot(s)), state_style(s))
}

// ── Тревога ──────────────────────────────────────────────────────────────────

/// Текст, окрашенный по уровню тревоги, — цифра отклика и всё, что меряется.
///
/// Порог задаёт справочник (`view::ping`), цвет выбирает оболочка. «Норма
/// молчит» и «нет ответа» в терминале одинаково тихие: тревога — цветная.
pub fn alarm_text(text: impl std::fmt::Display, a: Alarm) -> Span<'static> {
    let c = match a {
        Alarm::Amber => AMBER,
        Alarm::Red => RED,
        Alarm::Calm | Alarm::Muted => DIM,
    };
    Span::styled(text.to_string(), Style::default().fg(c))
}

/// Тост — сообщение внизу экрана, СО СТЕПЕНЬЮ ТРЕВОГИ.
///
/// Раньше тост был всегда мятным: «Пароль обновлён» и «автозапуск не настроен»
/// выглядели одинаково хорошо. Уровень берём из общего справочника
/// (`view::Alarm`), а не заводим свой: янтарь — беда, которая пройдёт сама
/// (переспросить, набрать заново), красный — которая сама не пройдёт.
///
/// Спокойный тост мятный, а не приглушённый: в отличие от цифры отклика, где
/// норма молчит, тост появляется только когда есть что сказать.
pub fn toast(msg: &str, a: Alarm) -> Span<'static> {
    let c = match a {
        Alarm::Calm => ACCENT,
        Alarm::Amber => AMBER,
        Alarm::Red => RED,
        Alarm::Muted => DIM,
    };
    Span::styled(msg.to_string(), Style::default().fg(c))
}

// ── Текст ────────────────────────────────────────────────────────────────────

/// Основной текст: значение настройки, имя годного хоста.
pub fn value(text: impl std::fmt::Display) -> Span<'static> {
    Span::styled(text.to_string(), Style::default().fg(FG))
}

/// Приглушённое: подсказка внизу экрана, пояснение рядом с состоянием, тихая
/// приписка, имя хоста, к которому всё равно не подключиться.
///
/// Одна деталь на всё второстепенное. Подсказка внизу была написана тремя
/// разными способами — в главном экране, в окне ввода и под QR.
pub fn hint(text: impl std::fmt::Display) -> Span<'static> {
    Span::styled(text.to_string(), Style::default().fg(DIM))
}

/// Код сети — то, что человек диктует или переписывает.
pub fn code(text: impl std::fmt::Display) -> Span<'static> {
    Span::styled(text.to_string(), Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))
}

/// Живая цифра под работающим состоянием (гости, часы сеанса): та же мята, что
/// и у «работает», но без нажима — это данные, а не заголовок.
pub fn live(text: impl std::fmt::Display) -> Span<'static> {
    Span::styled(text.to_string(), Style::default().fg(ACCENT))
}

// ── Рамки ────────────────────────────────────────────────────────────────────

/// Общая рамка: скруглённая, тусклый контур. Одна на все три вида рамок ниже.
fn frame<'a>() -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(DIM))
}

/// Карточка: рамка, акцентный жирный заголовок и поля по бокам, чтобы текст не
/// липнул к рамке.
pub fn card(title: &str) -> Block<'_> {
    frame()
        .title(Span::styled(title, Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)))
        .padding(Padding::horizontal(1))
}

/// Шапка приложения: та же рамка, но заголовок составной (в него входит значок
/// связи), а полей нет — внутри вкладки, а не текст.
pub fn head(title: Line<'_>) -> Block<'_> {
    frame().title(title)
}

/// Окно ввода поверх экрана: та же рамка, но контур акцентом — оно в фокусе, и
/// это единственное, чему сейчас достанутся нажатия.
pub fn modal(title: &str) -> Block<'_> {
    frame().border_style(Style::default().fg(ACCENT)).title(title).padding(Padding::horizontal(1))
}

// ── Строки списков ───────────────────────────────────────────────────────────

/// Ширина подписи в строке настройки — ОДНА на все вкладки.
///
/// Было две: 14 на «Хосте» и 16 на «Сервере», отчего значения на двух соседних
/// вкладках стояли в разных колонках. 14 хватает самой длинной подписи («Лимит
/// гостей», 12 знаков) с зазором в два пробела.
const LABEL_W: usize = 14;

/// Строка настройки «подпись — значение».
pub fn frow(label: &str, val: impl std::fmt::Display) -> ListItem<'static> {
    ListItem::new(setting_line(label, val))
}

/// То же содержимое отдельно от `ListItem` — у него содержимое приватное, а
/// колонку подписи надо чем-то проверять.
fn setting_line(label: &str, val: impl std::fmt::Display) -> Line<'static> {
    Line::from(vec![hint(format!("{label:<w$}", w = LABEL_W)), value(val)])
}

/// Строка-действие («▶ Стать хостом») — в списке настроек она одна, потому и
/// выделена.
pub fn action(text: &str) -> ListItem<'_> {
    ListItem::new(Line::from(code(text)))
}

/// Подсветка выбранной строки списка: фон-ступень, сам текст остаётся собой.
///
/// Цвет текста подсветка НЕ перебивает. У списка хостов раньше перебивала, и
/// выбранная строка теряла свои смыслы: красная цифра отклика и приглушённое
/// имя забитого хоста становились обычным текстом ровно у той строки, на
/// которую человек смотрит.
pub fn selected() -> Style {
    Style::default().bg(SEL).add_modifier(Modifier::BOLD)
}

/// Выбранная вкладка — чип акцентом.
pub fn tab_active() -> Style {
    Style::default().fg(BG).bg(ACCENT).add_modifier(Modifier::BOLD)
}

/// Строка набора в окне ввода: приглашение, набранное, курсор.
///
/// Приглашение и курсор — акцент без нажима: жирным они спорили бы с самим
/// набранным, а смотрят как раз на него.
pub fn prompt(typed: &str) -> Line<'static> {
    let mark = Style::default().fg(ACCENT);
    Line::from(vec![Span::styled("› ", mark), value(typed), Span::styled("▏", mark)])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ЗНАЧКИ СОСТОЯНИЯ — ОДНОЙ ШИРИНЫ.
    ///
    /// Разнобой не падает и не краснеет: строка просто едет на знак влево, и
    /// заметить это можно только сложив четыре состояния рядом. Так уже было —
    /// у «выключено» стоял голый U+26AA (ширина 1) против ширины 2 у соседей.
    ///
    /// Меряем тем же, чем меряет отрисовка (`Span::width` — та же
    /// `unicode-width`, что раскладывает буфер), а не глазами.
    #[test]
    fn every_state_dot_is_the_same_width() {
        let all = [State::On, State::Wait, State::Off, State::Err];
        for s in all {
            assert_eq!(
                Span::raw(state_dot(s)).width(),
                2,
                "{s:?}: значок шириной не 2 — строка с ним поедет мимо соседних"
            );
        }
    }

    /// Строка состояния всегда начинается со значка своего состояния.
    #[test]
    fn a_state_line_wears_its_own_dot() {
        for s in [State::On, State::Wait, State::Off, State::Err] {
            assert!(state_line(s, "текст").content.starts_with(state_dot(s)));
        }
        // Разные состояния — разные значки: иначе смысл не различить.
        let dots = [State::On, State::Wait, State::Off, State::Err].map(state_dot);
        for (i, a) in dots.iter().enumerate() {
            for b in &dots[i + 1..] {
                assert_ne!(a, b, "два состояния показываются одним значком");
            }
        }
    }

    /// Тревога у тоста РАЗЛИЧИМА: ошибка не смеет выглядеть как успех.
    ///
    /// Ровно это и было сломано — тост красился мятой всегда, и «автозапуск не
    /// настроен» читался как «готово».
    #[test]
    fn a_failed_toast_does_not_look_like_a_successful_one() {
        let calm = toast("готово", Alarm::Calm).style.fg;
        let amber = toast("наберите заново", Alarm::Amber).style.fg;
        let red = toast("не вышло", Alarm::Red).style.fg;
        assert_ne!(calm, amber);
        assert_ne!(calm, red);
        assert_ne!(amber, red);
    }

    /// Уровень тревоги переводится в состояние без выдумок: обрыв — не поломка.
    #[test]
    fn a_broken_link_is_not_shown_as_a_failure() {
        assert_eq!(State::from(Alarm::Calm), State::On);
        assert_eq!(State::from(Alarm::Amber), State::Wait);
        assert_eq!(State::from(Alarm::Red), State::Err);
        assert_eq!(State::from(Alarm::Muted), State::Off);
        // Справочник велит янтарь на обрыве связи — значок обязан пойти за ним.
        let (_, alarm) = bmv_common::view::link_state(Some(false));
        assert_eq!(state_dot(State::from(alarm)), state_dot(State::Wait));
    }

    /// Подпись в строке настройки — одной ширины на всех вкладках.
    #[test]
    fn every_setting_row_lines_up() {
        // Самая длинная подпись обеих вкладок ещё оставляет зазор.
        for label in ["Имя", "Лимит гостей", "Видимость", "Координатор", "Свой порт"] {
            let w = setting_line(label, "значение").spans[0].width();
            assert_eq!(w, LABEL_W, "подпись «{label}» встала не в ту колонку");
        }
    }
}
