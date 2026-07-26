// Компилирует ui/app.slint в Rust-код на этапе сборки (slint-build), а под Windows
// ещё встраивает манифест (UAC requireAdministrator + HiDPI) через embed-resource.
fn main() {
    slint_build::compile("ui/app.slint").expect("не удалось скомпилировать Slint UI");

    // Только когда ЦЕЛЬ сборки — Windows (не хост): встроить манифест приложения.
    // Без него не было бы автозапроса UAC, и VPN не смог бы создать TUN/маршруты.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // MSVC-линкер по умолчанию сам вшивает манифест с level=asInvoker. Рядом с
        // нашим requireAdministrator это конфликт двух манифестов, и «asInvoker» мог
        // бы победить → автозапрос UAC не сработает. Запрещаем линкеру генерировать
        // свой — остаётся ровно один манифест (наш, из bemyvpn.rc).
        println!("cargo:rustc-link-arg=/MANIFEST:NO");
        // Падаем громко: без манифеста не будет автозапроса UAC, и VPN не сможет
        // создать TUN/маршруты — лучше не собрать, чем собрать заведомо нерабочее.
        if let embed_resource::CompilationResult::Failed(e) =
            embed_resource::compile("bemyvpn.rc", embed_resource::NONE)
        {
            panic!("не удалось встроить Windows-манифест: {e}");
        }
    }
}
