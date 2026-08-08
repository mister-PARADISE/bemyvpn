//! ЧАСОВОЙ: хост не ищет СЕБЯ в общем каталоге.
//!
//! Поломка, ради которой он написан. Окно и терминал показывали свой адрес и своё
//! число гостей, находя собственную запись в каталоге хостов: `list.iter().find(
//! |h| h.id == свой_код)`. У публичной раздачи это работало, у СКРЫТОЙ — никогда:
//! скрытого хоста в каталоге нет по определению (`build_announce`: пароль либо
//! «не показывать» ⇒ `public = false`, а сервер кладёт в каталог только
//! публичных). `find` не находил ничего, и человек видел прочерк вместо адреса и
//! ноль вместо гостей — то есть НЕ ВИДЕЛ, что к нему подключились.
//!
//! Каталог здесь неверен по построению, а не «иногда опаздывает»: свои сведения
//! у хоста есть на руках раньше, чем сервер о них узнает. Правило поэтому такое:
//! **своё берём у своего движка** (`BmvEngine::host_guests`, свой IP — «whoami»),
//! а каталог остаётся тем, чем он и является, — списком ЧУЖИХ.
//!
//! Приём одолжен у соседей (`one_place_per_rule.rs`, `host_copy_matches_everywhere.rs`):
//! дешевле поймать грепом, чем каждый раз ловить руками.

use std::path::{Path, PathBuf};

/// Корень репозитория. `CARGO_MANIFEST_DIR` = `<корень>/crates/bmv-common`.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap().to_path_buf()
}

/// Десктопные оболочки — те две, что показывают свою раздачу на своём экране.
/// Телефоны сюда не входят: они спрашивают сервер запросом `resolve`, а он
/// отдаёт и скрытый хост, поэтому пустоты у них не бывает.
const SHELLS: [&str; 2] = ["apps/bmv-gui/src/main.rs", "apps/bmv-cli/src/tui.rs"];

/// Слова, которыми оболочка ПИШЕТ свои же сведения о раздаче.
const OWN_INFO: [&str; 3] = ["set_host_ip(", "set_host_guests(", "host_guests"];

/// Слово, которым ищут запись в каталоге. Рядом со своими сведениями его быть
/// не должно: ровно в таком соседстве и жила поломка.
const CATALOG_LOOKUP: &str = ".find(";

/// Сколько строк вокруг считать «рядом». Оба прежних места умещались в это окно:
/// у окна между `get_host_code()` и `find` стояла одна строка, у терминала обе
/// половины лежали на одной.
const NEAR: usize = 4;

/// Куски, которые рисуют СВОЮ раздачу целиком: в них поиска по каталогу нет
/// вовсе — ни рядом, ни поодаль. Проверка по имени функции, а не по окну строк:
/// в терминале обе половины прежней поломки жили на одной строке, и раздвинуть
/// их на пять строк — это по-прежнему та же поломка.
const OWN_ONLY_FNS: [(&str, &str); 2] =
    [("apps/bmv-cli/src/tui.rs", "fn host_tab("), ("apps/bmv-gui/src/main.rs", "fn fill_host_card(")];

fn read(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(root.join(rel))
        .unwrap_or_else(|e| panic!("{rel}: не прочитался ({e}) — оболочка переехала, поправь путь в этом тесте"))
}

/// Комментарии не считаем: они как раз ОБЪЯСНЯЮТ прежний приём и цитируют его.
fn is_comment(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("//") || t.starts_with("/*") || t.starts_with("* ") || t.starts_with("*/")
}

#[test]
fn a_host_takes_its_own_numbers_from_itself_and_not_from_the_catalog() {
    let root = repo_root();
    let mut guilty = Vec::new();

    for rel in SHELLS {
        let src = read(&root, rel);
        let lines: Vec<&str> = src.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if is_comment(line) {
                continue;
            }
            let Some(word) = OWN_INFO.iter().find(|w| line.contains(**w)) else { continue };
            let from = i.saturating_sub(NEAR);
            let to = (i + NEAR + 1).min(lines.len());
            for (j, near) in lines[from..to].iter().enumerate() {
                if !is_comment(near) && near.contains(CATALOG_LOOKUP) {
                    guilty.push(format!(
                        "{rel}:{}: свои сведения («{word}») пишутся рядом с поиском по каталогу ({rel}:{})\n      {}\n      {}",
                        i + 1,
                        from + j + 1,
                        line.trim(),
                        near.trim(),
                    ));
                }
            }
        }
        // …и обратная половина правила: честный источник обязан быть позван.
        // Без этого запрет «не бери из каталога» выполняется и пустым экраном.
        assert!(
            src.contains("host_guests()"),
            "{rel}: не спрашивает у движка `host_guests()`. Своё число гостей хост знает сам — \
             живой учёт `active_guests` в bmv-core; каталог для этого неверный источник по \
             построению (скрытой раздачи в нём нет).",
        );
    }

    // Вторая половина: функции, целиком посвящённые своей раздаче, каталога не
    // касаются вообще.
    for (rel, marker) in OWN_ONLY_FNS {
        let src = read(&root, rel);
        let mut lines = src.lines().enumerate().skip_while(|(_, l)| !l.starts_with(marker));
        let (start, _) = lines.next().unwrap_or_else(|| {
            panic!("{rel}: нет функции «{marker}» — её переименовали, поправь имя в этом тесте")
        });
        for (i, line) in lines.take_while(|(_, l)| !l.starts_with('}')) {
            if !is_comment(line) && line.contains(CATALOG_LOOKUP) {
                guilty.push(format!(
                    "{rel}:{}: «{marker}» (строка {}) рисует СВОЮ раздачу, а ищет по каталогу\n      {}",
                    i + 1,
                    start + 1,
                    line.trim(),
                ));
            }
        }
    }

    assert!(
        guilty.is_empty(),
        "ХОСТ СНОВА ИЩЕТ СЕБЯ В КАТАЛОГЕ:\n  {}\n\
         Свои адрес и гости берутся у СВОЕГО движка, а не из общего списка: скрытая раздача в \
         каталог не попадает вовсе, и такой поиск оставляет человека с прочерком вместо адреса и \
         нулём вместо гостей — то есть он не видит, что к нему подключились.",
        guilty.join("\n  ")
    );
}

/// Одна цифра — одно место. Число живых гостей считается ровно там, откуда его
/// берут и экран, и анонс в каталоге: иначе они разъедутся молча, и человек
/// прочтёт в окне одно, а гость в списке — другое.
#[test]
fn the_live_guest_count_is_counted_in_exactly_one_place() {
    let root = repo_root();
    let src = read(&root, "crates/bmv-core/src/lib.rs");
    // Всё после `#[cfg(test)]` — тесты, там своя арифметика по тому же полю.
    let code = src.split("#[cfg(test)]").next().unwrap_or_default();
    let counts = code
        .lines()
        .filter(|l| !is_comment(l) && l.contains("active_guests.lock().len()"))
        .count();
    assert_eq!(
        counts, 1,
        "живых гостей считают в {counts} местах — должно быть одно (`BmvEngine::host_guests`). \
         Второе место незаметно разойдётся с первым: в каталоге будет одна цифра, на экране другая.",
    );
}
