package org.bemyvpn.ui

import androidx.compose.animation.animateContentSize
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Autorenew
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material.icons.filled.People
import androidx.compose.material.icons.filled.Public
import androidx.compose.material.icons.filled.QrCode2
import androidx.compose.material.icons.filled.Router
import androidx.compose.material.icons.filled.VisibilityOff
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material3.Icon
import androidx.compose.material3.Slider
import androidx.compose.material3.SliderDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import org.bemyvpn.AppState
import org.bemyvpn.Haptics
import org.bemyvpn.Native
import org.bemyvpn.Theme
import org.bemyvpn.protoDesc
import org.bemyvpn.protoName
import org.bemyvpn.uptimeText

/** Вкладка «Хост» — раздать свой интернет. */
@Composable
fun HostTab(app: AppState, bottomPad: Dp) {
    var showQR by remember { mutableStateOf(false) }
    val protos = listOf("noise", "noise-obfs", "plain")

    androidx.compose.runtime.LaunchedEffect(Unit) { app.ensureHostCode() }

    // Панель ВНЕ прокрутки — тот же язык, что у VPN и сервера. Код сети НЕ
    // показывается, пока раздача выключена: давать его некому.
    // ЦВЕТ СОСТОЯНИЯ, А НЕ ЦВЕТ ЭКРАНА. Выключено — dim: раньше здесь стоял
    // акцент, и после слияния зелёного с мятой кольцо панели горело бы одной
    // мятой и при «Раздача выключена», и при «Раздаю».
    val tint = when {
        app.starting -> Theme.amber
        app.hosting -> Theme.accent
        app.hostError != null -> Theme.red
        else -> Theme.dim
    }
    // Панель — НАЛОЖЕНИЕ поверх прокрутки: настройки идут во всю высоту и
    // уходят ПОД неё (см. FloatingPanelLayout).
    FloatingPanelLayout(panel = {
    PinnedPanel(tint) {
        val starting = app.starting
        val err = app.hostError
        if (app.hosting) {
            rememberSecondTick()
            StatusLine(Icons.Filled.Router, "Раздаю", tint, uptimeText(app.hostStartedAt))
            Text(
                app.hostCode.ifEmpty { "…" }, color = Theme.accent,
                fontSize = 26.sp, fontWeight = FontWeight.ExtraBold, fontFamily = FontFamily.Monospace,
                letterSpacing = 2.sp, modifier = Modifier.fillMaxWidth(),
                textAlign = androidx.compose.ui.text.style.TextAlign.Center,
                maxLines = 1, overflow = TextOverflow.Ellipsis,
            )
            // «Новый код» — действие редкое, ему хватает строчки.
            QuietButton(Icons.Filled.Autorenew, "Новый код") { app.newHostCode() }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                StatTile(
                    "ГОСТЕЙ", "${app.myHostInfo?.guests ?: 0} / ${app.myHostInfo?.max ?: app.hostMax}",
                    Modifier.weight(1f), symbol = Icons.Filled.People,
                )
                StatTile(
                    "ВИДИМОСТЬ",
                    if (app.hostPublic && app.hostPassword.isEmpty()) "публичный" else "по коду",
                    Modifier.weight(1f),
                    symbol = if (app.hostPublic && app.hostPassword.isEmpty()) Icons.Filled.Public else Icons.Filled.VisibilityOff,
                )
            }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                StatTile(
                    "ПРОТОКОЛ", protoName(app.hostProtocol), Modifier.weight(1f),
                    symbol = protoIcon(Native.nativeProtection(app.hostProtocol)),
                )
                CopyTile("ВАШ IP", app.myHostInfo?.ip?.ifEmpty { "—" } ?: "—", Modifier.weight(1f))
            }
            // Поделиться кодом — В НИЗУ панели, прямо под самим кодом.
            if (app.hostCode.isNotEmpty()) ShareButtons(app.hostCode) { showQR = true }
        } else {
            Column(
                Modifier.fillMaxWidth(),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                HeroCircle(tint = tint, icon = Icons.Filled.Router, pulsing = starting)
                Text(
                    when {
                        starting -> "Запускаюсь…"
                        err != null -> "Не удалось начать раздачу"
                        else -> "Раздача выключена"
                    },
                    color = Theme.fg, fontSize = 21.sp, fontWeight = FontWeight.ExtraBold,
                    maxLines = 1, overflow = TextOverflow.Ellipsis,
                )
                Text(
                    when {
                        starting -> "Пробиваю канал наружу…"
                        err != null -> err
                        else -> "Станьте выходной точкой для друзей"
                    },
                    color = if (err != null && !starting) Theme.red else Theme.dim,
                    fontSize = 13.sp, textAlign = androidx.compose.ui.text.style.TextAlign.Center,
                )
            }
        }
    }
    }) { panelH ->
    Column(
        Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = 20.dp)
            // Отступ = высота панели плюс ОСТАТОК ЗАВЕСЫ (18 гашения − 12 нижней
            // рамки, уже посчитанной в panelH). См. VpnTab.
            .padding(top = panelH + 6.dp, bottom = bottomPad),
        verticalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        Text(
            "Раздайте свой интернет: телефон станет выходной точкой для гостей.",
            color = Theme.dim, fontSize = 13.sp,
        )

        SectionLabel("Имя хоста (видно в каталоге)")
        BmvTextField(app.hostName, { app.hostName = it; app.applyHostDebounced() }, "Имя")

        // ПАРОЛЬ ⇒ СЕТЬ ВСЕГДА СКРЫТАЯ. Правило соблюдает ядро (см. build_announce),
        // но интерфейс об этом молчал: человек ставил пароль, переключатель
        // продолжал гореть «Публичный», и он был уверен, что сеть в списке, — а её
        // там нет. Показываем ФАКТ, а не сохранённое желание.
        val locked = app.hostPassword.isNotEmpty()
        val publicNow = app.hostPublic && !locked
        SectionLabel("Видимость")
        Row(
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            modifier = Modifier.alpha(if (locked) 0.55f else 1f),   // выбор сейчас недоступен
        ) {
            BigChip(Icons.Filled.Public, "Публичный", on = publicNow, Modifier.weight(1f)) {
                if (!locked) { app.hostPublic = true; app.applyHostNow() }
            }
            BigChip(Icons.Filled.VisibilityOff, "Скрытый", on = !publicNow, Modifier.weight(1f)) {
                if (!locked) { app.hostPublic = false; app.applyHostNow() }
            }
        }
        Hint(
            when {
                locked -> "С паролем сеть всегда скрыта: публичная карточка выдала бы её как раз тем, от кого вы закрылись. Подключение — по коду."
                publicNow -> "Виден всем в списке хостов — подключиться сможет любой."
                else -> "В списке не виден — подключиться можно только по коду выше."
            },
        )

        Row(Modifier.padding(top = 6.dp), verticalAlignment = Alignment.CenterVertically) {
            Text("Лимит гостей", color = Theme.dim, fontSize = 13.sp, fontWeight = FontWeight.Bold)
            Box(Modifier.weight(1f))
            Text(
                "${app.hostMax}", color = Theme.accent,
                fontSize = 17.sp, fontWeight = FontWeight.ExtraBold, fontFamily = FontFamily.Monospace,
            )
        }
        // Применяем ТОЛЬКО по отпусканию ползунка — иначе поток сокет-запросов.
        Slider(
            value = app.hostMax.toFloat(),
            onValueChange = { app.hostMax = it.toInt().coerceIn(1, 256) },
            onValueChangeFinished = { app.applyHostNow() },
            valueRange = 1f..256f,
            colors = SliderDefaults.colors(
                thumbColor = Theme.accent, activeTrackColor = Theme.accent,
                inactiveTrackColor = Color.White.copy(alpha = 0.12f),
            ),
        )
        // То же правило, что у чипов: выбранное — своя ступень плюс подкраска,
        // плюс рамка и цветная цифра. Невыбранная кнопка теперь на s1, как и
        // чипы видимости выше: раньше эти две соседние группы «невыбранного»
        // сидели на разных ступенях (s2 и s1) — на одном экране, в сорока
        // строках друг от друга.
        Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            listOf(4, 8, 16, 32, 64, 128).forEach { v ->
                val on = app.hostMax == v
                Box(
                    Modifier.weight(1f)
                        .background(if (on) Theme.picked() else Theme.card, RoundedCornerShape(10.dp))
                        .border(1.dp, if (on) Theme.edge() else Color.Transparent, RoundedCornerShape(10.dp))
                        .tappable { app.hostMax = v; app.applyHostNow() }
                        .padding(vertical = 8.dp),
                    contentAlignment = Alignment.Center,
                ) {
                    Text("$v", fontSize = 13.sp, fontWeight = FontWeight.Bold, color = if (on) Theme.accent else Theme.dim)
                }
            }
        }

        SectionLabel("Пароль (пусто = без пароля)")
        BmvTextField(app.hostPassword, { app.hostPassword = it; app.applyHostDebounced() }, "без пароля")

        SectionLabel("Протокол")
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            protos.forEach { pid ->
                // Значок — по УРОВНЮ ЗАЩИТЫ от моста, а не по имени протокола:
                // список имён в двух местах (подпись и картинка) и есть та щель,
                // где хост однажды показался незашифрованным при живом шифре.
                BigChip(
                    protoIcon(Native.nativeProtection(pid)), protoName(pid),
                    on = app.hostProtocol == pid, Modifier.weight(1f),
                ) {
                    app.hostProtocol = pid; app.applyHostNow()
                }
            }
        }
        Hint(protoDesc(app.hostProtocol), warn = app.hostProtocol == "plain")

        // Кнопки «Стать хостом» тут больше нет: она переехала в нав-бар, по
        // общему правилу «ячейка ведёт на вкладку, а на своей вкладке становится
        // включателем». Раньше главное действие лежало под всеми настройками —
        // до него надо было домотать.
        Text(
            "Раздача работает и в фоне — приложение можно сворачивать.",
            color = Theme.dim, fontSize = 12.sp,
        )
    }
    }

    if (showQR) QrSheet(code = app.hostCode) { showQR = false }
}

/** Толстый чип: значок сверху, название снизу — палец попадает не глядя.
 *
 *  Выбранный — общее правило приложения: своя ступень плюс подкраска
 *  (Theme.picked) плюс рамка и цветной текст. Сплошной градиент со свечением
 *  кричал сильнее самого выбора: таких чипов на вкладке подряд шесть, и экран
 *  из них выходил в синих плашках. */
@Composable
private fun BigChip(icon: ImageVector, name: String, on: Boolean, modifier: Modifier, tap: () -> Unit) {
    val ctx = LocalContext.current
    val tint = if (on) Theme.accent else Theme.dim
    Column(
        modifier
            .background(if (on) Theme.picked() else Theme.card, RoundedCornerShape(14.dp))
            .border(1.dp, if (on) Theme.edge() else Theme.hairline, RoundedCornerShape(14.dp))
            .tappable { Haptics.tap(ctx); tap() }
            .padding(vertical = 14.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(5.dp),
    ) {
        Icon(icon, null, Modifier.size(20.dp), tint = tint)
        Text(
            name, color = tint, fontSize = 13.sp, fontWeight = FontWeight.Bold,
            maxLines = 1, overflow = TextOverflow.Ellipsis,
        )
    }
}

/** Пояснение к выбранному варианту — мелко и ненавязчиво, янтарём если риск. */
@Composable
private fun Hint(t: String, warn: Boolean = false) {
    Text(
        t, color = if (warn) Theme.amber else Theme.dim, fontSize = 12.sp,
        modifier = Modifier.fillMaxWidth().animateContentSize(tween(200)),
    )
}
