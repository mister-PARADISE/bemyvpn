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
import androidx.compose.ui.draw.shadow
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
            // Тень плотнее и глубже: ближний край + отрыв от страницы.
            .shadow(22.dp, RoundedCornerShape(28.dp), spotColor = Color.Black.copy(alpha = 0.85f), ambientColor = Color.White.copy(alpha = 0.10f))
            .background(Color(0xFF161C2B).copy(alpha = 0.98f), RoundedCornerShape(28.dp))
            .border(1.dp, Color.White.copy(alpha = 0.06f), RoundedCornerShape(28.dp))
            .padding(6.dp),
        horizontalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        // «Сервер» — навигация всегда: у вкладки нет своего включателя. Зато на
        // ней горит состояние связи, видное С ЛЮБОЙ вкладки: красная точка «нет
        // связи», жёлтая «проверяю». Когда всё хорошо — точки нет: молчание и
        // есть «ок». Раньше об обрыве кричала полоса на вкладке VPN.
        Cell(
            app, Tab.SERVER, Icons.Filled.Dns, "Сервер", Modifier.weight(1f),
            live = when (app.serverOnline) {
                true -> null
                false -> Theme.red
                else -> Theme.amber
            },
        )
        VpnCell(app, Modifier.weight(1f))
        HostCell(app, Modifier.weight(1f))
    }
}

/** Радиус внутренней таблетки: концентричность — внешний R − отступ (28−6=22). */
private val innerR = 22.dp

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
    val pill by animateColorAsState(
        if (active) Theme.accent.copy(alpha = 0.16f) else Color.Transparent,
        tween(180), label = "pill",
    )
    val tint = if (active) Theme.accent else Theme.dim
    androidx.compose.foundation.layout.Box(modifier) {
        Column(
            Modifier
                .fillMaxWidth()
                .background(pill, RoundedCornerShape(innerR))
                .tappable { app.tab = t }
                .padding(vertical = 8.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(3.dp),
        ) {
            androidx.compose.foundation.layout.Box(Modifier.height(iconBox), contentAlignment = Alignment.Center) {
                Icon(icon, null, Modifier.size(21.dp), tint = tint)
            }
            Text(label, color = tint, fontSize = 11.sp, fontWeight = androidx.compose.ui.text.font.FontWeight.Bold)
        }
        if (live != null) {
            androidx.compose.foundation.layout.Box(
                Modifier
                    .align(Alignment.TopCenter)
                    .padding(start = 38.dp, top = 3.dp)
                    .size(11.dp)
                    .background(Color(0xFF161C2B), androidx.compose.foundation.shape.CircleShape)
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
            live = when (app.vpnState) { 0 -> null; 2 -> Theme.green; else -> Theme.amber },
        )
        app.vpnState == 0 -> Action(modifier, Icons.Filled.Bolt, "Старт", Theme.green) { app.quickConnect() }
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
            live = if (!busy) null else if (app.hosting) Theme.green else Theme.amber,
        )
        busy -> Action(
            modifier, Icons.Filled.Close,
            if (app.starting) "Отмена" else "Стоп", Theme.red,
        ) { app.stopHost() }
        else -> Action(modifier, Icons.Filled.PowerSettingsNew, "Раздать", Theme.green) { app.becomeHost() }
    }
}

/** Ячейка-включатель: подкраска и рамка в цвет состояния, без крикливой заливки. */
@Composable
private fun Action(modifier: Modifier, icon: ImageVector, label: String, hue: Color, tap: () -> Unit) {
    Column(
        modifier
            .background(hue.copy(alpha = 0.14f), RoundedCornerShape(innerR))
            .border(1.dp, hue.copy(alpha = 0.37f), RoundedCornerShape(innerR))
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
