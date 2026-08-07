// Компилирует ui/app.slint в Rust-код на этапе сборки (slint-build), а под Windows
// ещё встраивает манифест (UAC requireAdministrator + HiDPI) через embed-resource.
fn main() {
    // Имена элементов разметки в отладочной сборке. Без них тесты оболочки не
    // могут найти в окне ни одной карточки (`ElementHandle`), а именно они
    // сторожат вылет при переключении вкладок и умную прокрутку. В релизе
    // отключено: там это только лишний вес.
    // Отдельная ручка на случай `cargo test --release`: там профиль уже не debug,
    // и без неё тесты окна упали бы с жалобой Slint на отсутствие имён.
    println!("cargo:rerun-if-env-changed=SLINT_EMIT_DEBUG_INFO");
    let cfg = slint_build::CompilerConfiguration::new().with_debug_info(
        std::env::var("PROFILE").as_deref() == Ok("debug")
            || std::env::var("SLINT_EMIT_DEBUG_INFO").is_ok(),
    );
    slint_build::compile_with_config("ui/app.slint", cfg).expect("не удалось скомпилировать Slint UI");

    // Только когда ЦЕЛЬ сборки — Windows (не хост): встроить манифест приложения.
    // Без него не было бы автозапроса UAC, и VPN не смог бы создать TUN/маршруты.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // MSVC-линкер по умолчанию сам вшивает манифест с level=asInvoker. Рядом с
        // нашим requireAdministrator это конфликт двух манифестов, и «asInvoker» мог
        // бы победить → автозапрос UAC не сработает. Запрещаем линкеру генерировать
        // свой — остаётся ровно один манифест (наш, из bemyvpn.rc).
        println!("cargo:rustc-link-arg=/MANIFEST:NO");
        // Пересобирать ресурсы и при правке самого значка: `embed-resource` следит
        // только за .rc, а .ico подключён из него именем.
        println!("cargo:rerun-if-changed=bemyvpn.ico");
        // Падаем громко: без манифеста не будет автозапроса UAC, и VPN не сможет
        // создать TUN/маршруты — лучше не собрать, чем собрать заведомо нерабочее.
        if let embed_resource::CompilationResult::Failed(e) =
            embed_resource::compile("bemyvpn.rc", win_version_macros())
        {
            panic!("не удалось встроить Windows-манифест: {e}");
        }
    }
}

/// Версия для блока VERSIONINFO в bemyvpn.rc — двумя подстановками:
/// `BMV_VER_NUM` (четыре числа для поля FILEVERSION) и `BMV_VER_STR` (строка,
/// как её видит человек в свойствах файла).
///
/// Источник ОДИН И ТОТ ЖЕ, что у `bmv_common::version::VERSION` — переменная
/// BMV_VERSION от CI. Запасное «0.0-dev» повторяет version.rs дословно: свойства
/// файла не должны расходиться с ответом самой программы. Сторож в CI сверяет
/// их у готового exe, так что разъехаться молча они не смогут.
fn win_version_macros() -> [String; 2] {
    println!("cargo:rerun-if-env-changed=BMV_VERSION");
    let v = match std::env::var("BMV_VERSION") {
        Ok(v) if !v.is_empty() => v,
        _ => "0.0-dev".to_owned(),
    };

    // «1.37» → «1,37,0,0». Разбор такой же щадящий, как в version.rs: хвост после
    // первой не-цифры отбрасывается, недостающие места — нули. Ручной прогон CI
    // подставляет сюда имя ветки, и на нём это тоже не должно падать.
    let mut n = [0u32; 4];
    for (i, part) in v.trim_start_matches('v').split('.').take(4).enumerate() {
        let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
        n[i] = digits.parse().unwrap_or(0);
    }

    [
        format!("BMV_VER_NUM={},{},{},{}", n[0], n[1], n[2], n[3]),
        format!("BMV_VER_STR=\"{v}\""),
    ]
}
