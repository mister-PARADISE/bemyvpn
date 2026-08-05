package org.bemyvpn.ui

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.animateContentSize
import androidx.compose.animation.core.animateFloatAsState
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
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowForward
import androidx.compose.material.icons.filled.ContentPaste
import androidx.compose.material.icons.filled.ExpandLess
import androidx.compose.material.icons.filled.ChevronRight
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material.icons.filled.LockOpen
import androidx.compose.material.icons.filled.People
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
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.AnnotatedString
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
import org.bemyvpn.uptimeText
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.relocation.BringIntoViewRequester
import androidx.compose.foundation.relocation.bringIntoViewRequester
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.IntSize
import org.bemyvpn.Native
import org.bemyvpn.Ping

// ── Состояния VPN для показа ─────────────────────────────────────────────────
// НОМЕРА ВАРИАНТОВ view::Vpn — часть договора с мостом (bmv_vpn_kind), здесь им
// даны только имена, чтобы `when` читался. ЧТО каждое значит и какими словами это
// сказать, решает справочник: подпись экран берёт готовой (AppState.vpnText).
private const val VPN_CONNECTING = 1
private const val VPN_RECONNECTING = 2
private const val VPN_ON = 3

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
            // Отступ = высота панели плюс ОСТАТОК ЗАВЕСЫ: панель меряется вместе
            // со своей нижней рамкой в 12dp, а гашение уходит от контура на 18 —
            // значит содержимому остаётся отступить ещё 6, чтобы покоиться ровно
            // за краем гашения, а не в нём (подтенённым на пустом месте).
            .padding(top = panelH + 6.dp, bottom = bottomPad),
        verticalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        UpdateBanner(app)

        SectionLabel("Подключиться по коду")
        CodeField(app, code, { code = it })

        // «Сканировать QR» — ТА ЖЕ СТУПЕНЬ, ЧТО У ДВУХ КНОПОК КОДА (s2). Стояла
        // на s3 и была самым светлым пятном страницы, хотя это не главное
        // действие экрана. Ступень s1 («на ступень выше своей подложки», а
        // подложка здесь — страница) не годится: рядом лежит поле кода, оно
        // ровно на s1, и кнопка слилась бы с ним в одно пятно. Один экран —
        // одна ступень для кнопки.
        Row(
            Modifier
                .fillMaxWidth()
                .background(Theme.cardHi, RoundedCornerShape(12.dp))
                .border(1.dp, Theme.hairline, RoundedCornerShape(12.dp))
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
            // Пусто по двум разным причинам, и человеку нужно разное: ждать
            // связи или действовать. Что именно сказать, решает справочник —
            // здесь эта развилка была второй его копией.
            Text(
                Native.nativeEmptyDirectoryHint(triOnline(app.serverOnline)),
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
            ) { shown.forEach { h -> HostCard(app, h, bottomPad) } }
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
                .border(1.dp, Theme.hairline, RoundedCornerShape(10.dp))
                .pressable { clipboard.getText()?.text?.let(onCode) },
            contentAlignment = Alignment.Center,
        ) {
            Icon(Icons.Filled.ContentPaste, null, Modifier.size(18.dp), tint = Theme.dim)
        }
        Box(
            // Перейти — ТА ЖЕ СТУПЕНЬ, ЧТО У «ВСТАВИТЬ» (s2). Мята отсюда снята:
            // она обещает «работает/включено», а это просто переход. Ступень
            // выше соседки тут тоже была лишней — на одном экране выходило три
            // ступени кнопок; владелец посмотрел и выбрал одну. Старшинство
            // несут ДРУГИЕ признаки, и их два: кнопка шире (52 против 44) и глиф
            // в ней светлый (fg против dim у «вставить»).
            Modifier.size(width = 52.dp, height = 44.dp)
                .background(Theme.cardHi, RoundedCornerShape(10.dp))
                .border(1.dp, Theme.hairline, RoundedCornerShape(10.dp))
                .pressable { val c = code; onCode(""); app.connectByCode(c) },
            contentAlignment = Alignment.Center,
        ) {
            Icon(Icons.AutoMirrored.Filled.ArrowForward, null, Modifier.size(22.dp), tint = Theme.fg)
        }
    }
}

/** Чип недавнего хоста: маленький флаг слева, затем имя. Подсвечен ВЫБРАННЫЙ.
 *
 *  Тот же чип, что и везде: невыбранный на s1, выбранный — s1 плюс подкраска.
 *  Раньше у него была своя пара чисел (0.16 и рамка 0.5) и своя ступень (s2) —
 *  третий рецепт одного и того же. */
@Composable
private fun RecentChip(app: AppState, id: String, highlighted: Boolean) {
    val host = app.hostById(id)
    Row(
        Modifier
            .background(if (highlighted) Theme.picked() else Theme.card, RoundedCornerShape(16.dp))
            .border(1.dp, if (highlighted) Theme.edge() else Color.Transparent, RoundedCornerShape(16.dp))
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
    // СОСТОЯНИЕ ДЛЯ ПОКАЗА СКЛЕИЛ СПРАВОЧНИК (см. AppState.vpnKind). Экран его
    // только одевает: краску, значок и разметку выбирает по номеру, а подпись
    // берёт готовой. Раньше он толковал сырой статус ядра сам — и «Подключаюсь»
    // против «Переподключение» решалось здесь, отдельно от трёх других оболочек.
    val kind = app.vpnKind
    val tint by animateColorAsState(
        // ЦВЕТ СОСТОЯНИЯ, А НЕ ЦВЕТ ЭКРАНА. Выключено — dim: раньше здесь стоял
        // акцент, и после слияния зелёного с мятой кольцо панели горело бы одной
        // мятой и при «VPN выключен», и при «Подключено» — то есть отвечало бы
        // одинаково на единственный вопрос, ради которого сюда смотрят.
        when (kind) {
            VPN_ON -> Theme.accent
            VPN_CONNECTING, VPN_RECONNECTING -> Theme.amber
            else -> Theme.dim
        },
        tween(300), label = "vpnTint",
    )
    // ПОСЛЕДНИЙ ИЗВЕСТНЫЙ хост, а не только живой: плитки с его цифрами уходят
    // растворением, и гасить нужно ЕГО цифры, а не пустоту (`connectedTo`
    // обнуляется в тот же миг, что и состояние).
    val live = app.connectedTo?.let { app.hostById(it) }
    val known = remember { mutableStateOf<Host?>(null) }
    if (live != null) known.value = live
    val host = known.value

    PinnedPanel(tint) {
        val icon: ImageVector = when (kind) {
            VPN_ON -> Icons.Filled.VerifiedUser
            VPN_CONNECTING, VPN_RECONNECTING -> Icons.Filled.Shield
            else -> Icons.Filled.RemoveModerator
        }
        // Подпись — ГОТОВАЯ, из справочника. Имя хоста ей предпочитается только в
        // работе: оно полезнее слова, которое и так видно по цвету кольца (то же
        // правило в трёх других оболочках).
        val title = if (kind == VPN_ON) host?.name ?: app.connectedTo ?: app.vpnText else app.vpnText
        if (kind == VPN_ON) {
            // Подключено — круг уступает место пользе: сколько идёт, куда,
            // адрес хоста, гости и чем позвать друзей.
            StatusLine(Icons.Filled.VerifiedUser, title, tint, uptimeText(app.connectedSince))
            // Строка «страна · протокол» есть, только пока хост виден в каталоге
            // (он может из него выпасть, а канал остаться). Переход у неё ЯВНЫЙ —
            // растворение НА МЕСТЕ, без сдвига и без разъезжания соседей.
            AnimatedVisibility(live != null, enter = fadeIn(tween(160)), exit = fadeOut(tween(140))) {
                host?.let { h ->
                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(5.dp)) {
                        Text(countryLabel(h), color = Theme.dim, fontSize = 13.sp)
                        Text("·", color = Theme.dim, fontSize = 13.sp)
                        Icon(protoIcon(h.protection), null, Modifier.size(13.dp), tint = Theme.dim)
                        Text(h.protoName, color = Theme.dim, fontSize = 13.sp)
                    }
                }
            }
            ConnectedExtras(host, live != null, app.connectedTo.orEmpty(), showInvite)
        } else {
            // Показывать нечего, кроме статуса — круг честно занимает место.
            Column(
                Modifier.fillMaxWidth(),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                val busy = kind == VPN_CONNECTING || kind == VPN_RECONNECTING
                HeroCircle(tint = tint, icon = icon, pulsing = busy)
                Text(
                    title, color = Theme.fg, fontSize = 21.sp, fontWeight = FontWeight.ExtraBold,
                    maxLines = 1, overflow = TextOverflow.Ellipsis,
                )
                if (busy) {
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
                //
                // Строка живёт своей жизнью — сама гаснет через пять секунд, без
                // смены состояния, — поэтому переход у неё ЯВНЫЙ: растворение НА
                // МЕСТЕ. Текст запоминается: иначе на выходе гасить было бы
                // нечего, строка пропала бы рывком.
                val err = app.vpnError
                val lastErr = remember { mutableStateOf("") }
                if (err != null) lastErr.value = err
                AnimatedVisibility(err != null, enter = fadeIn(tween(160)), exit = fadeOut(tween(140))) {
                    Text(
                        lastErr.value,
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
private fun ConnectedExtras(host: Host?, live: Boolean, code: String, showInvite: (String) -> Unit) {
    // Живая инфа о хосте: IP (копируется тапом) + сколько сейчас гостей. Есть,
    // только пока хост виден в каталоге, — отсюда явное растворение НА МЕСТЕ.
    AnimatedVisibility(live && host != null, enter = fadeIn(tween(160)), exit = fadeOut(tween(140))) {
        host?.let { h ->
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                CopyTile("IP ХОСТА", h.ip.ifEmpty { "—" }, Modifier.weight(1f))
                StatTile("ГОСТЕЙ", "${h.guests} / ${h.max}", Modifier.weight(1f), symbol = Icons.Filled.People)
            }
        }
    }
    // Поделиться сетью — В НИЗУ панели, прямо под тем, что копируют.
    if (code.isNotEmpty()) {
        ShareButtons(code, showInvite)
        Text(
            "Позвать друзей в эту же сеть", color = Theme.dim, fontSize = 11.sp,
            textAlign = TextAlign.Center, modifier = Modifier.fillMaxWidth(),
        )
    }
}

// ── Карточка хоста ───────────────────────────────────────────────────────────

@OptIn(ExperimentalFoundationApi::class)
@Composable
fun HostCard(app: AppState, host: Host, clearance: Dp) {
    var password by remember(host.id) { mutableStateOf("") }
    val expanded = app.expandedId == host.id
    // РАСКРЫЛИ — ПОДТЯНУТЬ КАРТОЧКУ В ВИДИМОЕ. Прокрутка сама за выросшей
    // карточкой не идёт: у последней в списке всё, что ниже заголовка, уезжало
    // под нав-бар, и «Подключить» человек не видел вовсе — на снимке владельца
    // кнопку срезало ровно посередине.
    //
    // Просим показать карточку ВМЕСТЕ С НИЖНИМ ОТСТУПОМ прокрутки: сама по себе
    // она «видима» и упираясь низом в край экрана, то есть под баром. С отступом
    // низ встаёт ровно там же, где встаёт при прокрутке до конца, — на верхней
    // кромке гашения.
    val requester = remember { BringIntoViewRequester() }
    var cardSize by remember { mutableStateOf(IntSize.Zero) }
    val clearancePx = with(LocalDensity.current) { clearance.toPx() }
    // ЖДЁМ, ПОКА ВЫСОТА ВСТАНЕТ, ПО САМОЙ ВЫСОТЕ, А НЕ ПО ЧАСАМ. Раскрытие — это
    // ДВЕ анимации подряд: начинка разъезжается (AnimatedVisibility, 200мс), а
    // коробка догоняет её размер (animateContentSize, ещё 200мс со сдвигом).
    // Отмеренные «220мс» ловили карточку на середине пути — прокрутка
    // подтягивалась к промежуточному размеру и не доводила. Ключ по cardSize
    // перезапускает ожидание на каждое изменение: тронется дальше — ждём снова,
    // замерла на 80мс — двигаем один раз и уже по готовой высоте.
    LaunchedEffect(expanded, cardSize) {
        if (!expanded) return@LaunchedEffect
        delay(80)
        requester.bringIntoView(
            Rect(0f, 0f, cardSize.width.toFloat(), cardSize.height + clearancePx),
        )
    }
    // ВНУТРИ КАРТОЧКИ МЯТЫ НЕТ — ОДНИ СТУПЕНИ.
    //
    // Пробовали и так, и эдак: сперва мятой красили карточку целиком (цвет
    // доставался оболочке, а не содержимому), потом — плитки внутри (семь
    // подкрашенных ячеек читались как «всё это включено»). Осталась чистая
    // лестница: свёрнутая карточка s1 (L* 8.1), раскрытая s2 (11.6), плитки
    // внутри неё s3 (19.31). Все три различимы, и ни одна ничего не обещает.
    // «Раскрыта» читается ступенью и мятной рамкой (edgeSoft), признак не один.
    val bg by animateColorAsState(if (expanded) Theme.cardHi else Theme.card, tween(200), label = "cardBg")
    // Заливка плиток внутри: ЧИСТАЯ СТУПЕНЬ s3, БЕЗ ПОДКРАСКИ.
    //
    // Мята с плиток снята: в раскрытой карточке их семь, и семь подкрашенных
    // ячеек читались как «всё это включено», хотя это просто цифры о хосте.
    // Ступень s3 для того и заведена — она отличима от своей подложки (L* 7.71
    // над s2 раскрытой карточки), но остаётся нейтральной. Та же ступень лежит
    // под плитками панели состояния, так что по оттенку они одно и то же.
    val tileFill = Theme.tile
    val stroke by animateColorAsState(
        if (expanded) Theme.edgeSoft() else Theme.hairline,
        tween(200), label = "cardStroke",
    )

    Column(
        Modifier
            .fillMaxWidth()
            .bringIntoViewRequester(requester)
            .onSizeChanged { cardSize = it }
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
            //
            // СТУПЕНЬ СЧИТАЕТСЯ ОТ СВОЕЙ КАРТОЧКИ, А НЕ ВПИСАНА ЧИСЛОМ. Жёсткий
            // s2 совпадал с фоном РАСКРЫТОЙ карточки — плашка
            // пропадала целиком, от неё оставалась одна рамка.
            Box(
                Modifier.size(56.dp)
                    .background(if (expanded) Theme.tile else Theme.cardHi, RoundedCornerShape(14.dp))
                    .border(1.dp, Theme.hairline, RoundedCornerShape(14.dp)),
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
                    Icon(protoIcon(host.protection), null, Modifier.size(14.dp), tint = Theme.dim)
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
                    CopyTile("КОД", host.id, Modifier.weight(1f), fill = tileFill)
                    CopyTile("IP", host.ip.ifEmpty { "—" }, Modifier.weight(1f), fill = tileFill)
                }
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    StatTile("СТРАНА", countryLabel(host), Modifier.weight(1f), fill = tileFill)
                    StatTile("ГОСТЕЙ", "${host.guests} / ${host.max}", Modifier.weight(1f), fill = tileFill)
                    PingTile(app.pingOf[host.id] ?: Ping.measuring, Modifier.weight(1f), fill = tileFill)
                }
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    StatTile(
                        "ДОСТУП", if (host.hasPassword) "по паролю" else "открытый", Modifier.weight(1f),
                        symbol = if (host.hasPassword) Icons.Filled.Lock else Icons.Filled.LockOpen,
                        fill = tileFill,
                    )
                    StatTile(
                        "ПРОТОКОЛ", host.protoName, Modifier.weight(1f),
                        symbol = protoIcon(host.protection), fill = tileFill,
                    )
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
                    .background(if (frac < 0.8f) Theme.accent else Theme.amber, CircleShape),
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
    // Кнопка МЯТНАЯ: обновиться — действие хорошее, а не тревожное. При
    // ошибке — в цвете ошибки, иначе выглядела бы чужой деталью.
    val btn = if (err) Theme.red else Theme.accent

    Row(
        Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(14.dp))
            // Слабая подкраска, как у раскрытой карточки: плашка большая, и полная
            // сила на такой площади кричала бы. Прежние 0.09 прозрачным поверх
            // страницы давали L* 9.6 — ТЕМНЕЕ обычной карточки списка, и сообщение
            // о новой версии выглядело провалом в фоне.
            .background(Theme.touched(tint))
            .border(1.dp, Theme.edgeSoft(tint), RoundedCornerShape(14.dp))
            .heightIn(min = 62.dp)
            .padding(horizontal = 14.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = when (app.updateState) {
                1 -> "Скачиваю $ver…"
                2 -> "Готово — открываю установщик"
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
                    .background(Theme.picked(btn))
                    .border(1.dp, Theme.edge(btn), RoundedCornerShape(11.dp))
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
