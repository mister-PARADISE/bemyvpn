package org.bemyvpn.ui

import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Bolt
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Dns
import androidx.compose.material.icons.filled.PowerSettingsNew
import androidx.compose.material.icons.filled.Router
import androidx.compose.material.icons.filled.Shield
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import org.bemyvpn.AppState
import org.bemyvpn.Tab
import org.bemyvpn.Theme

/**
 * Нижний нав-бар — плавающая таблетка: Сервер | VPN | Хост.
 *
 * ПРАВИЛО БАРА: ячейка ведёт на свою вкладку, а когда ты УЖЕ на ней — становится
 * включателем этой вкладки. Так центр работал и раньше («VPN» → «Старт»), теперь
 * то же самое умеет «Хост». Тупика с возвратом при этом не возникает: с любой
 * вкладки видно, куда уйти.
 *
 * Подсветка спокойная — подкраска и рамка. Раньше действующая ячейка была залита
 * градиентом во всю яркость и перетягивала на себя весь экран.
 */
@Composable
fun NavBar(app: AppState, modifier: Modifier = Modifier) {
    Row(
        modifier
            .padding(horizontal = 18.dp)
            // Ореол — РАНЬШЕ отделки: рисуется под баром, а не поверх. `up` —
            // бар прижат снизу, значит гасим по ВЕРХНЕЙ кромке, а за блок ореол
            // уходит вниз, за край экрана.
            .floatHalo(floatRadius, up = true)
            // ВТОРОЙ ПАРЯЩИЙ СЛОЙ: фон — тот же, что у панели состояния
            // (floatSurface). Свой цвет, подобранный на глаз, разошёлся бы с
            // панелью при первой же правке темы.
            //
            // РАДИУС ОБЩИЙ С ПАНЕЛЬЮ. Здесь стояло 28 против её 22, и от этого
            // расходилось ГАШЕНИЕ: его ширина считается от радиуса.
            .floatSurface(floatRadius, Theme.hairlineFloat)
            .padding(6.dp),
        horizontalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        // «Сервер» — навигация всегда: у вкладки нет своего включателя. Зато на
        // ней горит состояние связи, видное С ЛЮБОЙ вкладки: красная точка «нет
        // связи», жёлтая «проверяю». Когда всё хорошо — точки нет: молчание и
        // есть «ок». Раньше об обрыве кричала полоса на вкладке VPN.
        Cell(
            app, Tab.SERVER, Icons.Filled.Dns, "Сервер", Modifier.weight(1f),
            // Обрыв — янтарь, как и «проверяю»: он чинится сам за секунды.
            live = if (app.serverOnline == true) null else Theme.amber,
        )
        VpnCell(app, Modifier.weight(1f))
        HostCell(app, Modifier.weight(1f))
    }
}

/** Радиус внутренней таблетки: концентричность — внешний R − отступ бара.
 *  Считается, а не вписан числом: внешний радиус стал общим с панелью. */
private val innerR = floatRadius - 6.dp

/** Высота коробки значка — фиксированная, чтобы бар не менял высоту от иконки. */
private val iconBox = 22.dp

@Composable
private fun Cell(
    app: AppState,
    t: Tab,
    icon: ImageVector,
    label: String,
    modifier: Modifier,
    /** Точка состояния: горит, только когда ячейка ведёт на ДРУГУЮ вкладку. */
    live: Color? = null,
) {
    val active = app.tab == t
    // ТО ЖЕ «ВЫБРАННОЕ», ЧТО И ВЕЗДЕ (Theme.picked), И ТЕПЕРЬ С РАМКОЙ.
    //
    // Ячейка сидит на парящей панели, но красится ступенью s1 с подкраской, как
    // чипы на странице: подкраска в полную силу поверх самой панели уронила бы
    // акцентную подпись до 3.88 при пороге 4.5. Общий цвет выбранного даёт 4.57,
    // а над панелью ячейка поднята на 2.4 по L* — видно, что она приподнята, и
    // она не спорит яркостью с панелью состояния наверху экрана. Рамка появилась
    // затем, чтобы «я на этой вкладке» отличалось не одним лишь цветом.
    val pill by animateColorAsState(
        if (active) Theme.picked() else Color.Transparent,
        tween(180), label = "pill",
    )
    val edge by animateColorAsState(
        if (active) Theme.edge() else Color.Transparent,
        tween(180), label = "pillEdge",
    )
    val tint = if (active) Theme.accent else Theme.dim
    androidx.compose.foundation.layout.Box(modifier) {
        Column(
            Modifier
                .fillMaxWidth()
                .background(pill, RoundedCornerShape(innerR))
                .border(1.dp, edge, RoundedCornerShape(innerR))
                .tappable { app.tab = t }
                .padding(vertical = 8.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(3.dp),
        ) {
            androidx.compose.foundation.layout.Box(Modifier.height(iconBox), contentAlignment = Alignment.Center) {
                Icon(icon, null, Modifier.size(21.dp), tint = tint)
            }
            // Одна строка: подпись у ячейки МЕНЯЕТСЯ («Хост» → «Раздать»), и без
            // ограничения длинная переносилась бы на вторую строку, меняя высоту
            // бара при переключении вкладок.
            Text(
                label, color = tint, fontSize = 11.sp, maxLines = 1,
                fontWeight = androidx.compose.ui.text.font.FontWeight.Bold,
            )
        }
        if (live != null) {
            androidx.compose.foundation.layout.Box(
                Modifier
                    .align(Alignment.TopCenter)
                    .padding(start = 38.dp, top = 3.dp)
                    .size(11.dp)
                    .background(Theme.float, androidx.compose.foundation.shape.CircleShape)
                    .padding(2.dp)
                    .background(live, androidx.compose.foundation.shape.CircleShape),
            )
        }
    }
}

@Composable
private fun VpnCell(app: AppState, modifier: Modifier) {
    when {
        app.tab != Tab.VPN -> Cell(
            app, Tab.VPN, Icons.Filled.Shield, "VPN", modifier,
            live = when (app.vpnState) { 0 -> null; 2 -> Theme.accent; else -> Theme.amber },
        )
        // КНОПКА И СОСТОЯНИЕ ОДНОГО ЦВЕТА — РАЗЛИЧАЕТ ФОРМА. Мята значит и
        // «это можно нажать», и «это работает»; отдельного зелёного больше нет,
        // потому что от мяты его было не отличить. Здесь мята одета КНОПКОЙ:
        // подкраска плюс рамка. Состояние носит другую форму — залитую точку у
        // соседней ячейки, значок в панели. «Стоп» остаётся красным: на кнопке
        // выхода красный понятен без обучения и читается как «прервать».
        app.vpnState == 0 -> Action(modifier, Icons.Filled.Bolt, "Старт", Theme.accent) { app.quickConnect() }
        else -> Action(
            modifier, Icons.Filled.Close,
            if (app.vpnState == 1) "Отмена" else "Стоп", Theme.red,
        ) { app.stop() }
    }
}

@Composable
private fun HostCell(app: AppState, modifier: Modifier) {
    val busy = app.hosting || app.starting
    when {
        app.tab != Tab.HOST -> Cell(
            app, Tab.HOST, Icons.Filled.Router, "Хост", modifier,
            live = if (!busy) null else if (app.hosting) Theme.accent else Theme.amber,
        )
        busy -> Action(
            modifier, Icons.Filled.Close,
            if (app.starting) "Отмена" else "Стоп", Theme.red,
        ) { app.stopHost() }
        // «Раздать» — акцент по тому же правилу, что и «Старт».
        else -> Action(modifier, Icons.Filled.PowerSettingsNew, "Раздать", Theme.accent) { app.becomeHost() }
    }
}

/** Ячейка-включатель: подкраска и рамка в цвет состояния, без крикливой заливки. */
@Composable
private fun Action(modifier: Modifier, icon: ImageVector, label: String, hue: Color, tap: () -> Unit) {
    Column(
        modifier
            .background(Theme.picked(hue), RoundedCornerShape(innerR))
            .border(1.dp, Theme.edge(hue), RoundedCornerShape(innerR))
            .tappable(tap)
            .padding(vertical = 8.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(3.dp),
    ) {
        androidx.compose.foundation.layout.Box(Modifier.height(iconBox), contentAlignment = Alignment.Center) {
            Icon(icon, null, Modifier.size(21.dp), tint = hue)
        }
        Text(label, color = hue, fontSize = 11.sp, maxLines = 1, fontWeight = androidx.compose.ui.text.font.FontWeight.Bold)
    }
}
