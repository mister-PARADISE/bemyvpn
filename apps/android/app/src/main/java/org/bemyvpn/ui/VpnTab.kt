package org.bemyvpn.ui

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.material.icons.filled.Close
import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.animateContentSize
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.animation.expandVertically
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.shrinkVertically
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.ui.draw.clip
import androidx.compose.foundation.border
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowForward
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material.icons.filled.ContentPaste
import androidx.compose.material.icons.filled.ExpandLess
import androidx.compose.material.icons.filled.ChevronRight
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material.icons.filled.LockOpen
import androidx.compose.material.icons.filled.People
import androidx.compose.material.icons.filled.QrCode2
import androidx.compose.material.icons.filled.QrCodeScanner
import androidx.compose.material.icons.filled.RemoveModerator
import androidx.compose.material.icons.filled.Shield
import androidx.compose.material.icons.filled.VerifiedUser
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
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.delay
import org.bemyvpn.AppState
import org.bemyvpn.Haptics
import org.bemyvpn.Host
import org.bemyvpn.Theme
import org.bemyvpn.countryLabel
import org.bemyvpn.hostFlag
import org.bemyvpn.protoName
import org.bemyvpn.uptimeText

/** Вкладка «VPN» — герой-статус, код сети, недавние, список хостов. */
@Composable
fun VpnTab(app: AppState, bottomPad: Dp, openScanner: () -> Unit) {
    var code by remember { mutableStateOf("") }
    var inviteCode by remember { mutableStateOf<String?>(null) }

    Column(
        Modifier
            .fillMaxWidth()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = 20.dp)
            .padding(top = 20.dp, bottom = bottomPad),
        verticalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        UpdateBanner(app)
        VpnHero(app) { inviteCode = it }

        SectionLabel("Подключиться по коду")
        CodeField(app, code, { code = it })

        // «Сканировать QR» — как кнопка-плитка на iOS.
        Row(
            Modifier
                .fillMaxWidth()
                .background(Theme.tile, RoundedCornerShape(12.dp))
                .border(1.dp, Color.White.copy(alpha = 0.08f), RoundedCornerShape(12.dp))
                .pressable(onTap = openScanner)
                .padding(vertical = 13.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.CenterHorizontally),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(Icons.Filled.QrCodeScanner, null, Modifier.size(18.dp), tint = Theme.accent)
            Text("Сканировать QR", color = Color.White, fontSize = 15.sp, fontWeight = FontWeight.Bold)
        }

        // Недавние показываем ТОЛЬКО пока они онлайн (есть в живом каталоге).
        val onlineRecent = app.recent.filter { app.hostById(it)?.online == true }
        if (onlineRecent.isNotEmpty()) {
            SectionLabel("Недавние")
            Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                onlineRecent.forEach { id ->
                    RecentChip(app, id, highlighted = id == app.connectedTo)
                }
            }
        }

        SectionLabel("Хосты")
        val shown = app.displayedHosts()
        // Связь с сервером потеряна, а список НЕ пуст: цифры и состав хостов ниже —
        // последние известные, то есть могут врать. Молчать нельзя, иначе
        // устаревший список выглядит как живой. Руками делать ничего не нужно —
        // клиент переподключается сам, об этом и говорим.
        val stale = app.serverOnline == false && shown.isNotEmpty()
        if (stale) StaleBanner()
        if (shown.isEmpty()) {
            Text(
                if (app.serverOnline == false) "Нет связи с сервером.\nПроверьте адрес во вкладке «Сервер»."
                else "Хостов пока нет.\nВведите код сети или поднимите свой во вкладке «Хост».",
                color = Theme.dim, fontSize = 14.sp, textAlign = TextAlign.Center,
                modifier = Modifier.fillMaxWidth().padding(vertical = 40.dp),
            )
        } else {
            // Данные не живые — показываем это САМИМ СПИСКОМ, а не ещё одним
            // элементом: приглушённое читается как «неактуально» мгновенно.
            val dim by animateFloatAsState(if (stale) 0.55f else 1f, tween(400), label = "staleDim")
            Column(
                Modifier.alpha(dim),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) { shown.forEach { h -> HostCard(app, h) } }
        }
    }

    inviteCode?.let { QrSheet(code = it) { inviteCode = null } }
}

/** Поле «КОД СЕТИ» + вставить из буфера + перейти. */
@Composable
private fun CodeField(app: AppState, code: String, onCode: (String) -> Unit) {
    val clipboard = LocalClipboardManager.current
    Row(
        Modifier
            .fillMaxWidth()
            .background(Theme.card, RoundedCornerShape(14.dp))
            .padding(5.dp),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        BmvTextField(
            code, onCode, "КОД СЕТИ", Modifier.weight(1f).padding(12.dp),
            keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.Characters, autoCorrect = false),
            boxed = false,
        )
        Box(
            Modifier.size(width = 44.dp, height = 44.dp)
                .background(Color(0xFF2E3B57), RoundedCornerShape(10.dp))
                .pressable { clipboard.getText()?.text?.let(onCode) },
            contentAlignment = Alignment.Center,
        ) {
            Icon(Icons.Filled.ContentPaste, null, Modifier.size(18.dp), tint = Color.White)
        }
        Box(
            Modifier.size(width = 48.dp, height = 44.dp)
                .background(Brush.horizontalGradient(listOf(Theme.accent, Theme.accent2)), RoundedCornerShape(10.dp))
                .pressable { val c = code; onCode(""); app.connectByCode(c) },
            contentAlignment = Alignment.Center,
        ) {
            Icon(Icons.AutoMirrored.Filled.ArrowForward, null, Modifier.size(18.dp), tint = Color.White)
        }
    }
}

/** Чип недавнего хоста: маленький флаг слева, затем имя. Подсвечен ВЫБРАННЫЙ. */
@Composable
private fun RecentChip(app: AppState, id: String, highlighted: Boolean) {
    val host = app.hostById(id)
    Row(
        Modifier
            .background(if (highlighted) Theme.accent.copy(alpha = 0.16f) else Theme.cardSel, RoundedCornerShape(16.dp))
            .border(1.dp, if (highlighted) Theme.accent.copy(alpha = 0.5f) else Color.Transparent, RoundedCornerShape(16.dp))
            .tappable { if (app.vpnState == 0) app.connectByCode(id) }
            .padding(horizontal = 13.dp, vertical = 9.dp),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(host?.let { hostFlag(it) } ?: "🌐", fontSize = 15.sp)
        Text(
            host?.name ?: id, fontSize = 13.sp, fontWeight = FontWeight.Bold,
            color = if (highlighted) Theme.accent else Theme.fg, maxLines = 1, overflow = TextOverflow.Ellipsis,
        )
    }
}

// ── Единый блок статуса: выключен → подключаюсь → подключено ─────────────────
// Один и тот же блок перетекает между состояниями (кольцо, значок, цвет),
// чтобы взгляд не искал статус в новом месте.

@Composable
private fun VpnHero(app: AppState, showInvite: (String) -> Unit) {
    val tint by animateColorAsState(
        when (app.vpnState) { 1 -> Theme.amber; 2 -> Theme.green; else -> Theme.accent },
        tween(300), label = "vpnTint",
    )
    val icon: ImageVector = when (app.vpnState) {
        1 -> Icons.Filled.Shield
        2 -> Icons.Filled.VerifiedUser
        else -> Icons.Filled.RemoveModerator
    }
    val host = app.connectedTo?.let { app.hostById(it) }
    val title = when (app.vpnState) {
        // Уже были подключены и снова состояние 1 → это авто-реконнект (сменилась
        // сеть), а не первый коннект. Показываем это честно.
        1 -> if (app.connectedSince != null) "Переподключение…" else "Подключаюсь…"
        2 -> host?.name ?: app.connectedTo ?: "Подключено"
        else -> "VPN выключен"
    }

    Column(
        Modifier
            .fillMaxWidth()
            .background(Brush.verticalGradient(listOf(Theme.panel, Theme.card)), RoundedCornerShape(22.dp))
            .border(1.dp, tint.copy(alpha = 0.2f), RoundedCornerShape(22.dp))
            .padding(vertical = 26.dp, horizontal = 18.dp)
            .animateContentSize(tween(300)),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        HeroCircle(tint = tint, icon = icon, pulsing = app.vpnState == 1, glow = app.vpnState == 2)

        Text(
            title, color = Theme.fg, fontSize = 21.sp, fontWeight = FontWeight.ExtraBold,
            maxLines = 1, overflow = TextOverflow.Ellipsis,
        )
        // Подпись — с вклеенным значком протокола, как на iOS.
        when (app.vpnState) {
            1 -> Text(host?.name ?: "Пробиваю канал к хосту", color = Theme.dim, fontSize = 13.sp, textAlign = TextAlign.Center)
            2 -> {
                if (host != null) {
                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(5.dp)) {
                        Text(countryLabel(host), color = Theme.dim, fontSize = 13.sp)
                        Text("·", color = Theme.dim, fontSize = 13.sp)
                        Icon(protoIcon(host.proto), null, Modifier.size(13.dp), tint = Theme.dim)
                        Text(protoName(host.proto), color = Theme.dim, fontSize = 13.sp)
                    }
                } else Text("Канал поднят", color = Theme.dim, fontSize = 13.sp)
            }
            else -> {
                val n = app.displayedHosts().size
                Text(
                    if (n == 0) "Введите код сети или поднимите свой хост"
                    else "Доступно хостов: $n · выберите ниже или жмите «Старт»",
                    color = Theme.dim, fontSize = 13.sp, textAlign = TextAlign.Center,
                )
            }
        }

        // Разовое сообщение об отказе: отдельно от vpnState, иначе фоновый опрос
        // статуса ядра его затирает (или, наоборот, оставляет навсегда).
        app.vpnError?.let { err ->
            Text(err, color = Theme.red, fontSize = 13.sp, textAlign = TextAlign.Center)
        }

        if (app.vpnState == 2) ConnectedExtras(app, showInvite)
    }
}

@Composable
private fun ConnectedExtras(app: AppState, showInvite: (String) -> Unit) {
    val clipboard = LocalClipboardManager.current
    val ctx = LocalContext.current
    val host = app.connectedTo?.let { app.hostById(it) }
    var copiedInvite by remember { mutableStateOf(false) }
    LaunchedEffect(copiedInvite) { if (copiedInvite) { delay(1300); copiedInvite = false } }

    // Время на связи — тикает раз в секунду.
    rememberSecondTick()
    Text(
        uptimeText(app.connectedSince), color = Theme.green,
        fontSize = 15.sp, fontWeight = FontWeight.Bold, fontFamily = FontFamily.Monospace,
    )
    // Живая инфа о хосте: IP (копируется тапом) + сколько сейчас гостей.
    if (host != null) {
        Row(Modifier.padding(top = 2.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            CopyTile("IP ХОСТА", host.ip.ifEmpty { "—" }, Modifier.weight(1f))
            StatTile("ГОСТЕЙ", "${host.guests} / ${host.max}", Modifier.weight(1f), symbol = Icons.Filled.People)
        }
    }
    app.connectedTo?.let { id ->
        Box(Modifier.fillMaxWidth().padding(top = 2.dp).height(1.dp).background(Theme.dim.copy(alpha = 0.15f)))
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            InviteButton(
                if (copiedInvite) Icons.Filled.Check else Icons.Filled.ContentCopy,
                if (copiedInvite) "Скопировано" else "Код сети",
                green = copiedInvite, Modifier.weight(1f),
            ) {
                clipboard.setText(AnnotatedString(id)); Haptics.success(ctx); copiedInvite = true
            }
            InviteButton(Icons.Filled.QrCode2, "QR-код", green = false, Modifier.weight(1f)) { showInvite(id) }
        }
        Text("Позвать друзей в эту же сеть", color = Theme.dim, fontSize = 11.sp)
    }
}

/** Кнопка приглашения — во всю ширину, значок акцентом, как плитки. */
@Composable
private fun InviteButton(icon: ImageVector, title: String, green: Boolean, modifier: Modifier, tap: () -> Unit) {
    val tint = if (green) Theme.green else Theme.accent
    Row(
        modifier
            .background(Theme.tile, RoundedCornerShape(12.dp))
            .border(1.dp, if (green) Theme.green.copy(alpha = 0.5f) else Color.White.copy(alpha = 0.08f), RoundedCornerShape(12.dp))
            .pressable(onTap = tap)
            .padding(vertical = 11.dp),
        horizontalArrangement = Arrangement.spacedBy(6.dp, Alignment.CenterHorizontally),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(icon, null, Modifier.size(15.dp), tint = tint)
        Text(title, color = tint, fontSize = 13.sp, fontWeight = FontWeight.Bold)
    }
}

// ── Карточка хоста ───────────────────────────────────────────────────────────

@Composable
fun HostCard(app: AppState, host: Host) {
    var password by remember(host.id) { mutableStateOf("") }
    val expanded = app.expandedId == host.id
    val bg by animateColorAsState(if (expanded) Theme.cardHi else Theme.card, tween(200), label = "cardBg")
    val stroke by animateColorAsState(
        if (expanded) Theme.accent.copy(alpha = 0.28f) else Color.White.copy(alpha = 0.05f),
        tween(200), label = "cardStroke",
    )

    Column(
        Modifier
            .fillMaxWidth()
            .background(bg, RoundedCornerShape(16.dp))
            .border(1.dp, stroke, RoundedCornerShape(16.dp))
            .padding(14.dp)
            .animateContentSize(tween(200)),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Row(
            Modifier.fillMaxWidth().tappable {
                app.expandedId = if (expanded) null else host.id
                // Раскрыли — меряем ПОКА открыто; закрыли — прекращаем.
                app.watchPing(if (app.expandedId == host.id) host else null)
            },
            horizontalArrangement = Arrangement.spacedBy(12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            // Флаг страны крупно слева — как аватарка, на всю высоту строки.
            Box(
                Modifier.size(56.dp)
                    .background(Theme.tile, RoundedCornerShape(14.dp))
                    .border(1.dp, Color.White.copy(alpha = 0.08f), RoundedCornerShape(14.dp)),
                contentAlignment = Alignment.Center,
            ) { Text(hostFlag(host), fontSize = 29.sp) }

            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(5.dp)) {
                Text(
                    host.name.ifEmpty { host.id }, color = Theme.fg,
                    fontSize = 16.sp, fontWeight = FontWeight.SemiBold, maxLines = 1, overflow = TextOverflow.Ellipsis,
                )
                // Подпись: страна/IP · гостей N/M (+ значок «Скрытного»).
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                    val cc = org.bemyvpn.GeoFlags.countryOf(host.ip)
                    val parts = buildList {
                        if (cc != null) add(cc) else if (host.ip.isNotEmpty()) add(host.ip)
                        add("гостей ${host.guests}/${host.max}")
                    }
                    Text(parts.joinToString(" · "), color = Theme.dim, fontSize = 13.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    if (host.proto == "noise-obfs") {
                        Text("·", color = Theme.dim, fontSize = 13.sp)
                        Icon(protoIcon(host.proto), null, Modifier.size(13.dp), tint = Theme.dim)
                    }
                }
                CapacityBar(host)
            }
            if (host.hasPassword) Icon(Icons.Filled.Lock, null, Modifier.size(15.dp), tint = Theme.dim)
            Icon(
                if (expanded) Icons.Filled.ExpandLess else Icons.Filled.ChevronRight,
                null, Modifier.size(17.dp), tint = Theme.dim,
            )
        }

        AnimatedVisibility(
            expanded,
            enter = expandVertically(tween(200)) + fadeIn(tween(200)),
            exit = shrinkVertically(tween(200)) + fadeOut(tween(150)),
        ) {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                // Код и IP — тап копирует.
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    CopyTile("КОД", host.id, Modifier.weight(1f))
                    CopyTile("IP", host.ip.ifEmpty { "—" }, Modifier.weight(1f))
                }
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    StatTile("СТРАНА", countryLabel(host), Modifier.weight(1f))
                    StatTile("ГОСТЕЙ", "${host.guests} / ${host.max}", Modifier.weight(1f))
                    PingTile(app.pings[host.id] ?: "…", Modifier.weight(1f))
                }
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    StatTile(
                        "ДОСТУП", if (host.hasPassword) "по паролю" else "открытый", Modifier.weight(1f),
                        symbol = if (host.hasPassword) Icons.Filled.Lock else Icons.Filled.LockOpen,
                    )
                    StatTile("ПРОТОКОЛ", protoName(host.proto), Modifier.weight(1f), symbol = protoIcon(host.proto))
                }

                if (host.hasPassword) {
                    Box(Modifier.fillMaxWidth().background(Theme.tile, RoundedCornerShape(11.dp)).padding(12.dp)) {
                        BmvTextField(password, { password = it }, "Пароль", secure = true, boxed = false)
                    }
                }
                GradientButton("Подключить", enabled = host.usable) { app.connect(host, password) }
            }
        }
    }
}

/** Насколько хост заполнен — тонкая полоса под подписью. */
@Composable
private fun CapacityBar(host: Host) {
    if (host.max <= 0) return
    val frac = (host.guests.toFloat() / host.max).coerceIn(0f, 1f)
    Box(Modifier.fillMaxWidth().padding(top = 1.dp).height(3.dp).background(Color.White.copy(alpha = 0.09f), CircleShape)) {
        if (frac > 0f) {
            Box(
                Modifier
                    .fillMaxWidth(frac)
                    .height(3.dp)
                    .background(if (frac < 0.8f) Theme.green else Theme.amber, CircleShape),
            )
        }
    }
}


/**
 * Плашка «доступна новая версия». Тонкая полоса над героем: заметно, но не
 * требует реакции — крестик убирает её до следующего запуска. Навязываться
 * обновлением неправильно, решает человек.
 */
@Composable
private fun UpdateBanner(app: AppState) {
    val ver = app.updateVersion ?: return
    if (app.updateDismissed) return
    val err = app.updateState == 3
    val tint = if (err) Theme.red else Theme.accent

    Row(
        Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(14.dp))
            .background(tint.copy(alpha = 0.12f))
            .border(1.dp, tint.copy(alpha = 0.35f), RoundedCornerShape(14.dp))
            .padding(start = 14.dp, end = 8.dp)
            .height(46.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = when (app.updateState) {
                1 -> "Скачиваю $ver…"
                2 -> "Открываю установщик…"
                3 -> app.updateError
                else -> "Версия $ver доступна"
            },
            color = tint,
            fontSize = 13.sp,
            fontWeight = FontWeight.Bold,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
        // Во время загрузки кнопок нет: нажимать нечего, а повторный тап
        // запускал бы вторую загрузку поверх первой.
        if (app.updateState != 1 && app.updateState != 2) {
            Box(
                Modifier
                    .padding(start = 8.dp)
                    .clip(RoundedCornerShape(10.dp))
                    .background(Theme.accent)
                    .clickable { app.doUpdate() }
                    .padding(horizontal = 16.dp, vertical = 7.dp),
            ) {
                Text(
                    if (err) "Ещё раз" else "Обновить",
                    color = Color.White, fontSize = 12.sp, fontWeight = FontWeight.Bold,
                )
            }
        }
        if (app.updateState != 1) {
            Box(
                Modifier
                    .padding(start = 4.dp)
                    .size(30.dp)
                    .clip(RoundedCornerShape(8.dp))
                    .clickable { app.updateDismissed = true },
                contentAlignment = Alignment.Center,
            ) {
                Icon(Icons.Filled.Close, contentDescription = "Скрыть", tint = Theme.dim, modifier = Modifier.size(15.dp))
            }
        }
    }
}

/// Полоса «связь потеряна, восстанавливаю». Мягко ДЫШИТ — это и есть весь
/// индикатор процесса: отдельной «крутилки» не нужно, движение само говорит,
/// что работа идёт и руками ничего делать не надо.
@Composable
private fun StaleBanner() {
    val t = rememberInfiniteTransition(label = "stale")
    val a by t.animateFloat(
        1f, 0.72f,
        infiniteRepeatable(tween(1100), RepeatMode.Reverse),
        label = "staleBreath",
    )
    Row(
        Modifier.fillMaxWidth().alpha(a)
            .background(Color(0xFF3A2A15), RoundedCornerShape(10.dp))
            .border(1.dp, Theme.amber, RoundedCornerShape(10.dp))
            .padding(10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            "Нет связи с сервером — список ниже может устареть. Восстанавливаю связь…",
            color = Theme.amber, fontSize = 12.sp,
        )
    }
}

/// Плитка отклика: пока ждём первый ответ — мягко пульсирует, дальше просто
/// меняет цифру. Пульс ТОЛЬКО на ожидании: крутить его постоянно значит намекать,
/// что что-то грузится, хотя число уже есть и живёт своей жизнью.
@Composable
private fun PingTile(value: String, modifier: Modifier = Modifier) {
    val waiting = value == "…"
    val t = rememberInfiniteTransition(label = "ping")
    val pulse by t.animateFloat(
        1f, 0.45f,
        infiniteRepeatable(tween(700), RepeatMode.Reverse),
        label = "pingPulse",
    )
    StatTile("ОТКЛИК", value, modifier.alpha(if (waiting) pulse else 1f))
}
