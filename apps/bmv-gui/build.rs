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
            embed_resource::compile("bemyvpn.rc", embed_resource::NONE)
        {
            panic!("не удалось встроить Windows-манифест: {e}");
        }
    }
}
