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
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import org.bemyvpn.AppState
import org.bemyvpn.Tab
import org.bemyvpn.Theme

/**
 * Корень UI: фон, активная вкладка, плавающий нав-бар поверх снизу —
 * структура ContentView.swift (ZStack alignment bottom).
 */
@Composable
fun App(app: AppState, openScanner: () -> Unit) {
    val navInset = WindowInsets.navigationBars.asPaddingValues().calculateBottomPadding()
    // Контент не должен прятаться под плавающим баром (navPadding на iOS = 104pt).
    // Бар приподняли на 8dp — отступ подрос на ту же величину.
    val bottomPad = navInset + 104.dp
    Box(Modifier.fillMaxSize().background(Theme.bg)) {
        Box(Modifier.fillMaxSize().statusBarsPadding()) {
            when (app.tab) {
                Tab.SERVER -> ServerTab(app, bottomPad)
                Tab.VPN -> VpnTab(app, bottomPad, openScanner)
                Tab.HOST -> HostTab(app, bottomPad)
            }
        }
        // Бар приподнят над краем (как 42pt на iOS), но уважает системную панель:
        // прибавка идёт СВЕРХ безопасной зоны, поэтому на устройствах с
        // индикатором и без него ощущение одно.
        NavBar(app, Modifier.align(Alignment.BottomCenter).padding(bottom = navInset + 18.dp))
    }
}
