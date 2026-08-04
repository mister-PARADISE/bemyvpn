package org.bemyvpn.ui

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.material.icons.filled.Autorenew
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.SignalWifiOff
import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.animateContentSize
import androidx.compose.animation.core.LinearEasing
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
import androidx.compose.foundation.layout.fillMaxSize
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
import androidx.compose.ui.draw.rotate
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
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.heightIn
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

    // Панель — НАЛОЖЕНИЕ поверх прокрутки: список хостов идёт во всю высоту и
    // уходит ПОД неё (см. FloatingPanelLayout).
    FloatingPanelLayout(panel = { VpnHero(app) { inviteCode = it } }) { panelH ->
    Column(
        Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = 20.dp)
            // Отступ РОВНО в высоту панели: содержимое начинается сразу под ней.
            .padding(top = panelH + 2.dp, bottom = bottomPad),
        verticalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        UpdateBanner(app)

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

        val shown = app.displayedHosts()
        // Связь с сервером потеряна, а список НЕ пуст: цифры и состав хостов ниже —
        // последние известные, то есть могут врать. Молчать нельзя, иначе
        // устаревший список выглядит как живой. Руками делать ничего не нужно —
        // клиент переподключается сам, об этом и говорим.
        val stale = app.serverOnline == false && shown.isNotEmpty()
        // Знак у заголовка вместо полосы во всю ширину: та кричала сильнее, чем
        // стоило сообщение, и вдобавок дышала, перетягивая взгляд с самого
        // списка. Где искать поломку, показывает точка на ячейке «Сервер» в
        // нав-баре — видная с любой вкладки.
        // Заголовок здесь НЕ через SectionLabel: тот растянут на всю ширину и,
        // попав в строку, съедал всё место — знак «последние известные»
        // выталкивало за край экрана, и о потере связи ничего не сообщалось.
        Row(
            Modifier.fillMaxWidth().padding(top = 6.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text("Хосты", color = Theme.dim, fontSize = 13.sp, fontWeight = FontWeight.Bold)
            Spacer(Modifier.weight(1f))
            if (stale) StateChip("последние известные")
        }
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
                .background(Theme.cardHi, RoundedCornerShape(10.dp))
                .border(1.dp, Color.White.copy(alpha = 0.08f), RoundedCornerShape(10.dp))
                .pressable { clipboard.getText()?.text?.let(onCode) },
            contentAlignment = Alignment.Center,
        ) {
            Icon(Icons.Filled.ContentPaste, null, Modifier.size(18.dp), tint = Theme.dim)
        }
        Box(
            // Подкраска вместо сплошного синего: он спорил с акцентом всего
            // экрана. Стрелка крупнее соседней — здесь она главная.
            Modifier.size(width = 52.dp, height = 44.dp)
                .background(Theme.accent.copy(alpha = 0.16f), RoundedCornerShape(10.dp))
                .border(1.dp, Theme.accent.copy(alpha = 0.4f), RoundedCornerShape(10.dp))
                .pressable { val c = code; onCode(""); app.connectByCode(c) },
            contentAlignment = Alignment.Center,
        ) {
            Icon(Icons.AutoMirrored.Filled.ArrowForward, null, Modifier.size(22.dp), tint = Theme.accent)
        }
    }
}

/** Чип недавнего хоста: маленький флаг слева, затем имя. Подсвечен ВЫБРАННЫЙ. */
@Composable
private fun RecentChip(app: AppState, id: String, highlighted: Boolean) {
    val host = app.hostById(id)
    Row(
        Modifier
            .background(if (highlighted) Theme.accent.copy(alpha = 0.16f) else Theme.cardHi, RoundedCornerShape(16.dp))
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

    PinnedPanel(tint) {
        if (app.vpnState == 2) {
            // Подключено — круг уступает место пользе: сколько идёт, куда,
            // адрес хоста, гости и чем позвать друзей.
            StatusLine(Icons.Filled.VerifiedUser, title, tint, uptimeText(app.connectedSince))
            if (host != null) {
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(5.dp)) {
                    Text(countryLabel(host), color = Theme.dim, fontSize = 13.sp)
                    Text("·", color = Theme.dim, fontSize = 13.sp)
                    Icon(protoIcon(host.proto), null, Modifier.size(13.dp), tint = Theme.dim)
                    Text(protoName(host.proto), color = Theme.dim, fontSize = 13.sp)
                }
            }
            ConnectedExtras(app, showInvite)
        } else {
            // Показывать нечего, кроме статуса — круг честно занимает место.
            Column(
                Modifier.fillMaxWidth(),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                HeroCircle(tint = tint, icon = icon, pulsing = app.vpnState == 1, glow = false)
                Text(
                    title, color = Theme.fg, fontSize = 21.sp, fontWeight = FontWeight.ExtraBold,
                    maxLines = 1, overflow = TextOverflow.Ellipsis,
                )
                if (app.vpnState == 1) {
                    Text(host?.name ?: "Пробиваю канал к хосту", color = Theme.dim, fontSize = 13.sp, textAlign = TextAlign.Center)
                } else {
                    val n = app.displayedHosts().size
                    Text(
                        if (n == 0) "Введите код сети или поднимите свой хост"
                        else "Доступно хостов: $n · выберите ниже или жмите «Старт»",
                        color = Theme.dim, fontSize = 13.sp, textAlign = TextAlign.Center,
                    )
                }
                // Разовое сообщение: отдельно от vpnState, иначе фоновый опрос
                // статуса ядра его затирает (или оставляет навсегда). Красный —
                // только для настоящего отказа; штатно завершённая раздача идёт
                // тем же приглушённым цветом, что и строка над ней.
                app.vpnError?.let { err ->
                    Text(
                        err,
                        color = if (app.vpnNoticeCalm) Theme.dim else Theme.red,
                        fontSize = 13.sp,
                        textAlign = TextAlign.Center,
                    )
                }
            }
        }
    }
}

@Composable
private fun ConnectedExtras(app: AppState, showInvite: (String) -> Unit) {
    val host = app.connectedTo?.let { app.hostById(it) }
    // Живая инфа о хосте: IP (копируется тапом) + сколько сейчас гостей.
    if (host != null) {
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            CopyTile("IP ХОСТА", host.ip.ifEmpty { "—" }, Modifier.weight(1f))
            StatTile("ГОСТЕЙ", "${host.guests} / ${host.max}", Modifier.weight(1f), symbol = Icons.Filled.People)
        }
    }
    // Поделиться сетью — В НИЗУ панели, прямо под тем, что копируют.
    app.connectedTo?.let { id ->
        ShareButtons(id, showInvite)
        Text(
            "Позвать друзей в эту же сеть", color = Theme.dim, fontSize = 11.sp,
            textAlign = TextAlign.Center, modifier = Modifier.fillMaxWidth(),
        )
    }
}

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
            //
            // ПЛАШКА НА СТУПЕНЬ НИЖЕ ПЛИТКИ (s2, а не s3), САМ ФЛАГ ПРИГЛУШЁН.
            // Флаг — единственное полностью насыщенное пятно на экране, и сидел
            // он на самой светлой подложке темы: в списке глаз ловил его раньше
            // имени хоста, ради которого на строку и смотрят.
            Box(
                Modifier.size(56.dp)
                    .background(Theme.cardHi, RoundedCornerShape(14.dp))
                    .border(1.dp, Color.White.copy(alpha = 0.08f), RoundedCornerShape(14.dp)),
                contentAlignment = Alignment.Center,
            ) { Text(hostFlag(host), fontSize = 29.sp, modifier = Modifier.alpha(0.85f)) }

            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(5.dp)) {
                // ЗНАЧОК ПРОТОКОЛА — В СТРОКЕ ИМЕНИ, ПЕРЕД НИМ.
                //
                // Значок есть у КАЖДОГО протокола: раньше он был только у
                // «Маскировки», и его отсутствие означало сразу две
                // противоположные вещи — «Обычный» (шифр есть) и «Без шифра»
                // (шифра нет); незащищённый хост выглядел в списке ровно как
                // защищённый. Стоял он в строке подписи, под именем; теперь
                // рядом с самим именем — защита относится к хосту, а не к
                // счётчику гостей.
                //
                // ПЕРВЫМ, а не в хвосте: Row меряет детей по порядку, и значок
                // в конце в узкой строке просто выдавливался бы за край (имя с
                // ellipsis успевало бы забрать всю ширину). Слева значки
                // выстраиваются в ровный столбец, а многоточие съедает хвост
                // имени — как ему и положено. weight(1f, fill = false) на имени
                // и есть та самая уступка ширины.
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(5.dp)) {
                    Icon(protoIcon(host.proto), null, Modifier.size(14.dp), tint = Theme.dim)
                    Text(
                        host.name.ifEmpty { host.id }, color = Theme.fg,
                        modifier = Modifier.weight(1f, fill = false),
                        fontSize = 16.sp, fontWeight = FontWeight.SemiBold, maxLines = 1, overflow = TextOverflow.Ellipsis,
                    )
                }
                // Подпись: страна · гостей N/M. Значка здесь больше нет.
                //
                // СЫРОГО IP в подписи нет: он стоял первым (когда страна не
                // определилась) и один занимал всю ширину — до счётчика гостей
                // многоточие не доходило. Адрес виден в раскрытой карточке, там
                // под него отдельная плитка. Счётчик гостей есть ВСЕГДА — это и
                // делает подпись непустой при любых данных.
                run {
                    val cc = org.bemyvpn.GeoFlags.countryOf(host.ip)
                    val parts = buildList {
                        if (cc != null) add(cc)
                        // Потолок хост может и не объявить (0) — дробь «1/0» врёт.
                        add(if (host.max > 0) "гостей ${host.guests}/${host.max}" else "гостей ${host.guests}")
                    }
                    Text(parts.joinToString(" · "), color = Theme.dim, fontSize = 13.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
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
                CalmButton("Подключить", enabled = host.usable) { app.connect(host, password) }
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
    val err = app.updateState == 3
    val tint = if (err) Theme.red else Theme.accent
    // Кнопка ЗЕЛЁНАЯ: обновиться — действие хорошее, а не тревожное. Синяя
    // сливалась с акцентом интерфейса и не читалась как «нажми меня». При
    // ошибке — в цвете ошибки, иначе выглядела бы чужой деталью.
    val btn = if (err) Theme.red else Theme.green

    Row(
        Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(14.dp))
            .background(tint.copy(alpha = 0.09f))
            .border(1.dp, tint.copy(alpha = 0.28f), RoundedCornerShape(14.dp))
            .heightIn(min = 62.dp)
            .padding(horizontal = 14.dp, vertical = 12.dp),
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
            modifier = Modifier.weight(1f),
        )
        // Во время загрузки кнопок нет: нажимать нечего, а повторный тап
        // запускал бы вторую загрузку поверх первой.
        if (app.updateState != 1 && app.updateState != 2) {
            Box(
                Modifier
                    .padding(start = 14.dp)
                    .height(38.dp)
                    .clip(RoundedCornerShape(11.dp))
                    .background(btn.copy(alpha = 0.14f))
                    .border(1.dp, btn.copy(alpha = 0.4f), RoundedCornerShape(11.dp))
                    .clickable { app.doUpdate() }
                    .padding(horizontal = 18.dp),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    if (err) "Ещё раз" else "Обновить",
                    color = btn, fontSize = 13.sp, fontWeight = FontWeight.Bold,
                )
            }
        }
        // Крестика нет намеренно: это единственный канал про новую версию, и
        // спрятав его, человек больше о ней не узнает.
    }
}

/// Полоса «связь потеряна, восстанавливаю». Мягко ДЫШИТ — это и есть весь
/// индикатор процесса: отдельной «крутилки» не нужно, движение само говорит,
/// что работа идёт и руками ничего делать не надо.

/**
 * Плитка отклика.
 *
 *   • идёт первый замер — КРУГОВАЯ СТРЕЛКА ВРАЩАЕТСЯ: движение честно говорит
 *     «сейчас меряю», в отличие от многоточия, которое просто стоит и молчит;
 *   • ответа нет — перечёркнутая антенна: у «нет отклика» отдельный знак, а не
 *     прочерк, который легко принять за «данных нет»;
 *   • число есть — просто цифра, без анимации: крутить что-то поверх готового
 *     значения значит намекать, что оно ещё не готово.
 */
@Composable
private fun PingTile(value: String, modifier: Modifier = Modifier) {
    when (value) {
        "…" -> {
            val t = rememberInfiniteTransition(label = "ping")
            val angle by t.animateFloat(
                0f, 360f,
                infiniteRepeatable(tween(900, easing = LinearEasing), RepeatMode.Restart),
                label = "pingSpin",
            )
            // Через trailing у TileBody, чтобы не менять общий StatTile ради
            // одного случая: вращение нужно только здесь.
            Box(modifier.tileBackground()) {
                TileBody("ПИНГ", "") {
                    Icon(
                        Icons.Filled.Autorenew, null,
                        Modifier.size(14.dp).rotate(angle),
                        tint = Theme.accent,
                    )
                }
            }
        }
        // Только знак, без слова: перечёркнутая антенна говорит сама, а «нет»
        // рядом с ней — это то же самое ещё раз.
        "—" -> StatTile("ПИНГ", "", modifier, symbol = Icons.Filled.SignalWifiOff, tint = Theme.dim)
        else -> StatTile("ПИНГ", value, modifier, tint = pingTint(value))
    }
}

/// ОДНА ЛИНЕЙКА НА ОБА ПИНГА (здесь и на вкладке «Сервер»). Раньше их было две,
/// и меньшее число выходило тревожнее большего: 137 мс до хоста краснело, 162 мс
/// до координатора числилось «хорошо».
///
/// ЗЕЛЁНОГО В ПИНГЕ НЕТ ВОВСЕ: в этом приложении зелёный значит «работает», а не
/// «быстро». Норма молчит — обычный цвет текста.
private fun pingTint(value: String): Color {
    val ms = value.substringBefore(' ').toIntOrNull() ?: return Theme.fg
    return when {
        ms < 250 -> Theme.fg
        ms <= 500 -> Theme.amber
        else -> Theme.red
    }
}
