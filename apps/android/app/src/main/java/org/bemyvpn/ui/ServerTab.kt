package org.bemyvpn.ui

import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.PortableWifiOff
import androidx.compose.material.icons.filled.SettingsInputAntenna
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.text.style.TextAlign
import org.bemyvpn.AppState
import org.bemyvpn.Theme

/** Вкладка «Сервер» — статус координатора + смена адреса. */
@Composable
fun ServerTab(app: AppState, bottomPad: androidx.compose.ui.unit.Dp) {
    var coordField by remember { mutableStateOf(app.coordinator) }
    LaunchedEffect(app.coordinator) { coordField = app.coordinator }

    // Открыли вкладку — сразу свежая цифра, не дожидаясь очередного круга.
    // Сам цикл живёт всегда (см. watchServer), как на десктопе и iOS.
    LaunchedEffect(Unit) { app.checkServer() }

    // Панель — НАЛОЖЕНИЕ поверх прокрутки: настройки идут во всю высоту и
    // уходят ПОД неё (см. FloatingPanelLayout).
    FloatingPanelLayout(panel = { ServerHero(app) }) { panelH ->
    Column(
        Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = 20.dp)
            // Отступ РОВНО в высоту панели: содержимое начинается сразу под ней.
            .padding(top = panelH + 12.dp, bottom = bottomPad),
        verticalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        Text(
            "Сервер только помогает найти хостов и связаться с ними. Ваш трафик идёт напрямую к хосту, мимо сервера.",
            color = Theme.dim, fontSize = 12.sp, modifier = Modifier.fillMaxWidth(),
        )

        SectionLabel("Другой адрес сервера")
        BmvTextField(coordField, { coordField = it }, "http://адрес:3330")

        CalmButton("Сохранить и проверить") {
            app.saveCoordinator(coordField.ifEmpty { app.coordinator })
        }
        if (app.coordinator != app.defaultCoordinator) {
            Text(
                "Вернуть стандартный сервер", color = Theme.dim, fontSize = 13.sp,
                modifier = Modifier.fillMaxWidth().tappable {
                    coordField = app.defaultCoordinator
                    app.saveCoordinator(app.defaultCoordinator)
                },
                textAlign = androidx.compose.ui.text.style.TextAlign.Center,
            )
        }

        val hist = app.serverHistory.filter { it != app.coordinator }
        if (hist.isNotEmpty()) {
            SectionLabel("Недавние серверы")
            Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                hist.forEach { url ->
                    Text(
                        url.removePrefix("https://").removePrefix("http://"),
                        color = Theme.fg, fontSize = 13.sp, fontWeight = FontWeight.Bold,
                        modifier = Modifier
                            // Тот же чип, что и «Недавние» на вкладке VPN: на s1.
                            .background(Theme.card, RoundedCornerShape(16.dp))
                            .tappable { coordField = url; app.saveCoordinator(url) }
                            .padding(horizontal = 14.dp, vertical = 9.dp),
                    )
                }
            }
        }
    }
    }
}

/** Тот же герой, что на вкладках VPN и «Хост» — единый язык статуса. */
@Composable
private fun ServerHero(app: AppState) {
    // Кольцо отвечает на «сервер доступен?» — это ДА/НЕТ. Качество связи
    // показывает пинг отдельной плиткой.
    // ОБРЫВ СВЯЗИ — НЕ БЕДА, а янтарь. Он чинится сам за секунды, и кричать о
    // нём красным (кружок, значок, рамка вокруг самого большого блока экрана,
    // точка на нав-баре — четырьмя способами разом) значит расходовать цвет
    // тревоги на то, что пройдёт само. Красный остаётся там, где само не
    // пройдёт: ошибка настроек и ошибка подключения.
    val tint by animateColorAsState(
        if (app.serverOnline == true) Theme.accent else Theme.amber,
        tween(300), label = "srvTint",
    )
    val icon = if (app.serverOnline == false) Icons.Filled.PortableWifiOff else Icons.Filled.SettingsInputAntenna
    val statusText = when (app.serverOnline) {
        true -> "На связи"; false -> "Нет связи — восстанавливаю…"; null -> "Проверяю связь…"
    }
    // Та же линейка, что у пинга до хоста (см. pingTint в VpnTab).
    val pingColor = if (app.ping < 250) Theme.fg else if (app.ping <= 500) Theme.amber else Theme.red
    val addr = app.coordinator.removePrefix("https://").removePrefix("http://")

    // Когда связь есть, круг уступает место цифрам: смотреть на большой значок
    // «всё хорошо» смысла нет, а панель висит на экране постоянно.
    PinnedPanel(tint) {
        if (app.serverOnline == true) {
            StatusLine(icon, statusText, tint)
            Text(addr, color = Theme.dim, fontSize = 13.sp, fontFamily = FontFamily.Monospace)
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                // Обычная плитка, а не кнопка: проверка идёт сама каждую секунду
                // секунды, и нажатие экономило бы в лучшем случае их же.
                StatTile("ПИНГ", "${app.ping} мс", Modifier.weight(1f), tint = pingColor)
                StatTile("ХОСТОВ", "${app.hosts.size}", Modifier.weight(1f))
            }
            CopyTile("ВАШ IP", app.myIp.ifEmpty { "—" }, Modifier.fillMaxWidth())
        } else {
            Column(
                Modifier.fillMaxWidth(),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                HeroCircle(tint = tint, icon = icon, pulsing = app.checking)
                Text(statusText, color = Theme.fg, fontSize = 21.sp, fontWeight = FontWeight.ExtraBold, textAlign = TextAlign.Center)
                Text(addr, color = Theme.dim, fontSize = 13.sp, fontFamily = FontFamily.Monospace)
            }
        }
    }
}
