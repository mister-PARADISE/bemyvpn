package org.bemyvpn

import androidx.compose.ui.graphics.Color

/** Палитра — та же, что в iOS-приложении (Theme.swift), тёмная тема. */
object Theme {
    // ── ЛЕСТНИЦА ПОВЕРХНОСТЕЙ: ЧЕТЫРЕ СТУПЕНИ, А НЕ ОДИННАДЦАТЬ ──
    //
    // Замер по снимкам давал одиннадцать разных подложек, и порядок в них был
    // нарушен: парящая панель оказывалась ТЕМНЕЕ карточки под ней — оттого и не
    // парила. Ступеней теперь ровно четыре, и каждая значит одно:
    val bg      = Color(0xFF0B0E14)   // s0 — страница
    val card    = Color(0xFF161B26)   // s1 — карточка списка, поле, «поделиться»
    val cardHi  = Color(0xFF1C2434)   // s2 — раскрытая карточка, выбранное, чип
    val tile    = Color(0xFF242D3E)   // s3 — плитка внутри панели/карточки
    val accent  = Color(0xFF7BA6F0)
    val fg      = Color(0xFFEAECEF)
    val dim     = Color(0xFF99A1B4)
    val green   = Color(0xFF34E29E)
    val red     = Color(0xFFF2707E)
    val amber   = Color(0xFFF5B14C)
}
