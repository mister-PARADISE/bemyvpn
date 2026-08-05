package org.bemyvpn.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.asPaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.dp
import org.bemyvpn.AppState
import org.bemyvpn.Tab
import org.bemyvpn.Theme

/** На сколько бар приподнят над безопасной зоной (как 42pt на iOS). */
private val navLift = 18.dp

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
