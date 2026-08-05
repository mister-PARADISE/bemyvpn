package org.bemyvpn.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.asPaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.text.selection.LocalTextSelectionColors
import androidx.compose.foundation.text.selection.TextSelectionColors
import androidx.compose.material.ripple.LocalRippleTheme
import androidx.compose.material.ripple.RippleAlpha
import androidx.compose.material.ripple.RippleTheme
import androidx.compose.material3.LocalContentColor
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.dp
import org.bemyvpn.AppState
import org.bemyvpn.Tab
import org.bemyvpn.Theme

/** На сколько бар приподнят над безопасной зоной (как 42pt на iOS). */
private val navLift = 18.dp

/**
 * Рябь — мятная и тихая. Материалово-чёрной ей быть неоткуда: приложение живёт
 * без `MaterialTheme` (см. корень ниже), и без подмены сюда встаёт
 * `DebugRippleTheme` — чёрная, из отладочного умолчания Compose.
 *
 * Доли одинаковые у всех состояний, кроме наведения: пальцем состояний «навёл»
 * и «нажал» не различить, а мышью у нас пользуются только в окне, где рябь не
 * рисуется вовсе. Одно число вместо четырёх подобранных.
 */
private object BmvRipple : RippleTheme {
    @Composable override fun defaultColor(): Color = Theme.accent
    @Composable override fun rippleAlpha(): RippleAlpha =
        RippleAlpha(draggedAlpha = 0.12f, focusedAlpha = 0.12f, hoveredAlpha = 0.06f, pressedAlpha = 0.12f)
}

/**
 * Корень UI: фон, активная вкладка, плавающий нав-бар поверх снизу —
 * структура ContentView.swift (ZStack alignment bottom).
 */
@Composable
fun App(app: AppState, openScanner: () -> Unit) {
    val navInset = WindowInsets.navigationBars.asPaddingValues().calculateBottomPadding()
    val density = LocalDensity.current
    // ВЫСОТА БАРА МЕРЯЕТСЯ, А НЕ ВПИСАНА ЧИСЛОМ — так же, как высота панели
    // состояния (см. FloatingPanelLayout).
    //
    // Здесь стояло 106 = 18 подъёма + 70 бара + 18 завесы, и все три слагаемых
    // держались в уме: подняли бар — правь число, укоротили завесу — правь опять
    // (24→18 уже правили). А высоту бара не назначаем МЫ: она растёт от
    // системного масштаба шрифта — на крупном бар выше 70, и низ последней
    // карточки уезжал ему под завесу, вместе с кнопкой «Подключить».
    var barH by remember { mutableStateOf(0.dp) }
    // Низ ПОКОЯЩЕГОСЯ содержимого приходится ровно на ВЕРХ завесы бара: иначе
    // последняя карточка гаснет, стоя на месте.
    val bottomPad = navInset + navLift + barH + veilWidth
    // ── ЧЕМ КРАСИТ СИСТЕМА, КОГДА ЕЙ НЕ СКАЗАЛИ ─────────────────────────────
    //
    // Приложение НАРОЧНО живёт без `MaterialTheme` (из material3 взяты ровно
    // `Text`, `Icon` и `Slider`), и это правильно: цвета у нас свои. Но «нет
    // темы» не значит «нет цвета» — значит ДЕФОЛТ, а дефолты у Compose вполне
    // определённые и чужие:
    //
    //   • LocalTextSelectionColors → гугл-синий #4286F4: подсветка выделения и
    //     обе «капли»-хэндла в полях ввода. Каретку мы задали давно
    //     (`cursorBrush = Theme.accent`), а выделение — нет, и в одном поле
    //     жили мятный курсор и синяя подсветка;
    //   • LocalRippleTheme → чёрная рябь. Её рисует `SliderDefaults.Thumb` под
    //     пальцем — тёмный ореол вокруг мятного бегунка, и ни один параметр
    //     `SliderDefaults.colors` этого не покрывает;
    //   • LocalContentColor → Color.Black. Задевает то, что рисуется без явного
    //     цвета: у нас это запасной глиф 🌐 и любой флаг без эмодзи-шрифта —
    //     чёрным по почти чёрному, то есть никак.
    //
    // Три подмены отдаём ОДНИМ местом. Это дешевле и честнее, чем затащить
    // `colorScheme` целиком: у Material тридцать с лишним ролей, и заполнять их
    // нашими девятью цветами значило бы придумать два десятка значений, которых
    // в `design/palette.toml` нет.
    CompositionLocalProvider(
        LocalTextSelectionColors provides
            TextSelectionColors(handleColor = Theme.accent, backgroundColor = Theme.accent.copy(alpha = 0.4f)),
        LocalRippleTheme provides BmvRipple,
        LocalContentColor provides Theme.fg,
    ) {
        Box(Modifier.fillMaxSize().background(Theme.bg)) {
            Box(Modifier.fillMaxSize().statusBarsPadding()) {
                when (app.tab) {
                    Tab.SERVER -> ServerTab(app, bottomPad)
                    Tab.VPN -> VpnTab(app, bottomPad, openScanner)
                    Tab.HOST -> HostTab(app, bottomPad)
                }
            }
            // Бар приподнят над краем, но уважает системную панель: прибавка идёт
            // СВЕРХ безопасной зоны, поэтому на устройствах с индикатором и без него
            // ощущение одно. `onSizeChanged` стоит ПОСЛЕ отступа — меряет сам бар.
            NavBar(
                app,
                Modifier.align(Alignment.BottomCenter)
                    .padding(bottom = navInset + navLift)
                    .onSizeChanged { barH = with(density) { it.height.toDp() } },
            )
        }
    }
}
