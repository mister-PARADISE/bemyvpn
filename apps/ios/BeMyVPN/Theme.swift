import SwiftUI

/// Палитра — та же, что в Android-приложении (тёмная тема).
enum Theme {
    // ── ЛЕСТНИЦА ПОВЕРХНОСТЕЙ: ЧЕТЫРЕ СТУПЕНИ, А НЕ ОДИННАДЦАТЬ ──
    //
    // Замер по снимкам давал одиннадцать разных подложек, и порядок в них был
    // нарушен: парящая панель оказывалась ТЕМНЕЕ карточки под ней — оттого и не
    // парила. Ступеней теперь ровно четыре, и каждая значит одно:
    static let bg      = Color(hex: 0x0B0E14)   // s0 — страница
    static let card    = Color(hex: 0x161B26)   // s1 — карточка списка, поле, «поделиться»
    static let cardHi  = Color(hex: 0x1C2434)   // s2 — раскрытая карточка, выбранное, чип
    static let tile    = Color(hex: 0x242D3E)   // s3 — плитка внутри панели/карточки
    static let accent  = Color(hex: 0x7BA6F0)
    static let fg      = Color(hex: 0xEAECEF)
    static let dim     = Color(hex: 0x99A1B4)
    static let green   = Color(hex: 0x34E29E)
    static let red     = Color(hex: 0xF2707E)
    static let amber   = Color(hex: 0xF5B14C)
}

extension Color {
    init(hex: UInt32) {
        self.init(
            .sRGB,
            red:   Double((hex >> 16) & 0xFF) / 255,
            green: Double((hex >> 8) & 0xFF) / 255,
            blue:  Double(hex & 0xFF) / 255,
            opacity: 1
        )
    }
}
