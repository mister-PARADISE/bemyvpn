// Встраивает в Windows-exe сведения о файле (VERSIONINFO из bemyvpn.rc).
// На остальных ОС — ничего: там ресурсов нет как понятия.
fn main() {
    // Только когда ЦЕЛЬ сборки — Windows (не хост): иначе rc-компилятора нет и
    // искать его незачем.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // Падаем громко: молча выпустить exe с пустыми свойствами — ровно та
        // беда, от которой этот файл и заведён.
        if let embed_resource::CompilationResult::Failed(e) =
            embed_resource::compile("bemyvpn.rc", win_version_macros())
        {
            panic!("не удалось встроить сведения о файле: {e}");
        }
    }
}

/// Версия для блока VERSIONINFO в bemyvpn.rc — двумя подстановками:
/// `BMV_VER_NUM` (четыре числа для поля FILEVERSION) и `BMV_VER_STR` (строка,
/// как её видит человек в свойствах файла).
///
/// Тот же код, что в apps/bmv-gui/build.rs, и это осознанно: общий кусок
/// пришлось бы тащить через `include!` из чужой папки, а следить за его
/// изменениями — вручную (обе оболочки уже печатают `rerun-if`, а он отменяет
/// слежение за папкой целиком). Двадцать строк дешевле такой ловушки, а от
/// расхождения сторожит CI: он сверяет версию в свойствах готового exe с той,
/// что печатает сама программа.
fn win_version_macros() -> [String; 2] {
    println!("cargo:rerun-if-env-changed=BMV_VERSION");
    let v = match std::env::var("BMV_VERSION") {
        Ok(v) if !v.is_empty() => v,
        _ => "0.0-dev".to_owned(),
    };

    // «1.37» → «1,37,0,0»: хвост после первой не-цифры отбрасывается, недостающие
    // места — нули (ручной прогон CI подставляет сюда имя ветки).
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
