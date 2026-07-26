package org.bemyvpn

import androidx.compose.ui.graphics.Color

/** Палитра — та же, что в iOS-приложении (Theme.swift), тёмная тема. */
object Theme {
    val bg      = Color(0xFF0B0E14)
    val card    = Color(0xFF161B26)
    val cardSel = Color(0xFF1E2A44)
    val panel   = Color(0xFF121722)
    /** Слои внутри карточки хоста. В тёмной теме поверхность СВЕТЛЕЕТ по мере
     *  подъёма: страница → карточка → плитка. */
    val cardHi  = Color(0xFF1A2233)   // раскрытая карточка хоста
    val tile    = Color(0xFF242D3E)   // плитка внутри неё
    val accent  = Color(0xFF5E93FF)
    val accent2 = Color(0xFF3D6FE0)
    val fg      = Color(0xFFEAECEF)
    val dim     = Color(0xFF8B93A7)
    val green   = Color(0xFF34E29E)
    val red     = Color(0xFFFF5A6A)
    val amber   = Color(0xFFF5B14C)
}
