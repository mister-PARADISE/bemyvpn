import SwiftUI

/// Палитра — та же, что в Android-приложении (тёмная тема).
enum Theme {
    static let bg      = Color(hex: 0x0B0E14)
    static let card    = Color(hex: 0x161B26)
    static let cardSel = Color(hex: 0x1E2A44)
    static let panel   = Color(hex: 0x121722)
    /// Слои внутри карточки хоста. В тёмной теме поверхность СВЕТЛЕЕТ по мере
    /// подъёма: страница → карточка → плитка. Раньше плитки красились в `bg`
    /// (самый тёмный) и читались как дыры, пробитые в карточке.
    static let cardHi  = Color(hex: 0x1A2233)   // раскрытая карточка хоста
    static let tile    = Color(hex: 0x242D3E)   // плитка внутри неё
    static let accent  = Color(hex: 0x5E93FF)
    static let accent2 = Color(hex: 0x3D6FE0)
    static let fg      = Color(hex: 0xEAECEF)
    static let dim     = Color(hex: 0x8B93A7)
    static let green   = Color(hex: 0x34E29E)
    static let red     = Color(hex: 0xFF5A6A)
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
