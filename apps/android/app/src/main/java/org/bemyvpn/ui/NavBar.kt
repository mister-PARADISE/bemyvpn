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
import androidx.compose.material.icons.filled.Router
import androidx.compose.material.icons.filled.Shield
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import org.bemyvpn.AppState
import org.bemyvpn.Tab
import org.bemyvpn.Theme

/**
 * Нижний нав-бар — плавающая таблетка (как на iOS): Сервер | VPN | Хост.
 * Центральная ячейка на вкладке VPN — главное действие: «Старт» (зелёный
 * градиент) или «Стоп» (красный), на других вкладках — обычный переход на VPN.
 */
@Composable
fun NavBar(app: AppState, modifier: Modifier = Modifier) {
    Row(
        modifier
            .padding(horizontal = 18.dp)
            .shadow(14.dp, RoundedCornerShape(28.dp), spotColor = Color.Black.copy(alpha = 0.6f), ambientColor = Color.White.copy(alpha = 0.10f))
            .background(Color(0xFF161C2B).copy(alpha = 0.98f), RoundedCornerShape(28.dp))
            .border(1.dp, Color.White.copy(alpha = 0.06f), RoundedCornerShape(28.dp))
            .padding(6.dp),
        horizontalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Cell(app, Tab.SERVER, Icons.Filled.Dns, "Сервер", Modifier.weight(1f))
        VpnCell(app, Modifier.weight(1f))
        Cell(app, Tab.HOST, Icons.Filled.Router, "Хост", Modifier.weight(1f))
    }
}

/** Радиус внутренней таблетки: концентричность — внешний R − отступ (28−6=22). */
private val innerR = 22.dp

/** Высота коробки значка — фиксированная, чтобы бар не менял высоту от иконки. */
private val iconBox = 22.dp

@Composable
private fun Cell(app: AppState, t: Tab, icon: ImageVector, label: String, modifier: Modifier) {
    val active = app.tab == t
    val pill by animateColorAsState(
        if (active) Theme.accent.copy(alpha = 0.26f) else Color.Transparent,
        tween(180), label = "pill",
    )
    val tint = if (active) Theme.accent else Theme.dim
    Column(
        modifier
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
}

@Composable
private fun VpnCell(app: AppState, modifier: Modifier) {
    when {
        app.tab != Tab.VPN -> Cell(app, Tab.VPN, Icons.Filled.Shield, "VPN", modifier)
        app.vpnState == 0 -> Action(
            modifier, Icons.Filled.Bolt, "Старт",
            listOf(Color(0xFF34E29E), Color(0xFF12B07E)),
        ) { app.quickConnect() }
        else -> Action(
            modifier, Icons.Filled.Close, "Стоп",
            listOf(Color(0xFFFF6473), Color(0xFFE23B4C)),
        ) { app.stop() }
    }
}

@Composable
private fun Action(modifier: Modifier, icon: ImageVector, label: String, grad: List<Color>, tap: () -> Unit) {
    Column(
        modifier
            .shadow(8.dp, RoundedCornerShape(innerR), spotColor = grad[0].copy(alpha = 0.35f))
            .background(Brush.verticalGradient(grad), RoundedCornerShape(innerR))
            .tappable(tap)
            .padding(vertical = 8.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(3.dp),
    ) {
        androidx.compose.foundation.layout.Box(Modifier.height(iconBox), contentAlignment = Alignment.Center) {
            Icon(icon, null, Modifier.size(21.dp), tint = Color.White)
        }
        Text(label, color = Color.White, fontSize = 11.sp, fontWeight = androidx.compose.ui.text.font.FontWeight.Bold)
    }
}
