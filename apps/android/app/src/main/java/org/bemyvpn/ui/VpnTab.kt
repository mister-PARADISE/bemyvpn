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
import androidx.compose.foundation.ScrollState
import androidx.compose.foundation.gestures.animateScrollBy
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.layout.LayoutCoordinates
import androidx.compose.ui.layout.onGloballyPositioned
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
    val scroll = rememberScrollState()
    // Окно прокрутки МЕРЯЕМ, а не выводим из экрана: сверху его режет системная
    // строка состояния, и «высота экрана» ошиблась бы ровно на неё.
    var viewport by remember { mutableStateOf<LayoutCoordinates?>(null) }
    val reveal = rememberReveal(scroll, viewport, topInset = panelH + 6.dp, bottomInset = bottomPad)
    Box(Modifier.fillMaxSize().onGloballyPositioned { viewport = it }) {
    Column(
        Modifier
            .fillMaxSize()
            .verticalScroll(scroll)
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
        CodeField(app, code, { code = it }, openScanner)

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
            ) { shown.forEach { h -> HostCard(app, h, reveal) } }
        }
    }
    }
    }

    inviteCode?.let { QrSheet(code = it) { inviteCode = null } }
}

/**
 * ПОКАЗАТЬ РАСКРЫТУЮ КАРТОЧКУ ЦЕЛИКОМ. Одно правило на всю оболочку — и то же
 * самое, что на iPhone и в окне.
 *
 * ВИДИМАЯ ПОЛОСА — ЭТО НЕ ЭКРАН. Сверху её режет парящая панель состояния
 * (`topInset` — её замеренная высота плюс остаток завесы, тот же отступ, что у
 * содержимого), снизу — нав-бар со своим гашением (`bottomInset` = `bottomPad`,
 * тоже замеренный). Карточка, упёршаяся низом в край экрана, «видима» только на
 * бумаге: на деле она лежит под баром — на этом уже обжигались.
 *
 * Помещается в полосу — ЦЕНТРИРУЕМ: глазу не надо искать, куда уехала карточка.
 * Не помещается — центрировать НЕЛЬЗЯ, спрячется её начало вместе с именем
 * хоста; тогда прижимаем ВЕРХ карточки к верху полосы и показываем, сколько
 * влезло. Сдвиг зажимает сама прокрутка, поэтому первую карточку короткого
 * списка центрировать нечем — она честно остаётся на месте.
 */
@Composable
private fun rememberReveal(
    scroll: ScrollState,
    viewport: LayoutCoordinates?,
    topInset: Dp,
    bottomInset: Dp,
): suspend (LayoutCoordinates) -> Unit {
    val density = LocalDensity.current
    return { card ->
        val vp = viewport
        if (vp != null && vp.isAttached && card.isAttached) {
            val top = with(density) { topInset.toPx() }
            val bottom = vp.size.height - with(density) { bottomInset.toPx() }
            val cardH = card.size.height.toFloat()
            val band = bottom - top
            val want = if (cardH <= band) top + (band - cardH) / 2 else top
            // Где карточка СЕЙЧАС относительно окна прокрутки (сдвиг прокрутки
            // уже учтён самой раскладкой).
            val now = vp.localPositionOf(card, Offset.Zero).y
            scroll.animateScrollBy(now - want, tween(250))
        }
    }
}

/** Поле «КОД СЕТИ» + снять камерой + вставить из буфера + перейти. */
@Composable
private fun CodeField(app: AppState, code: String, onCode: (String) -> Unit, openScanner: () -> Unit) {
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
        // ТРИ ДЕЙСТВИЯ НАД ОДНИМ И ТЕМ ЖЕ — В ОДНОЙ СТРОКЕ. «Снять код камерой»
        // стояло отдельной широкой кнопкой под полем, хотя это такой же способ
        // НАПОЛНИТЬ поле, как «вставить», — и занимало целую строку экрана.
        //
        // Порядок: редкое слева, завершающее справа, у большого пальца. Скан
        // нужен реже всех (требует второго экрана рядом), поэтому он крайний
        // слева — и обе прежние кнопки остались там же, где к ним привыкли.
        // Краска глифа у скана `dim`, как у «вставить»: это два равных способа
        // наполнить поле, а мята обещала бы «работает/включено» (по той же
        // причине её сняли с «перейти»).
        CodeButton(Icons.Filled.QrCodeScanner, "Сканировать QR", onTap = openScanner)
        CodeButton(Icons.Filled.ContentPaste, "Вставить код") { clipboard.getText()?.text?.let(onCode) }
        // Перейти — ТА ЖЕ СТУПЕНЬ, ЧТО У «ВСТАВИТЬ» (s2). Мята отсюда снята:
        // она обещает «работает/включено», а это просто переход. Ступень
        // выше соседки тут тоже была лишней — на одном экране выходило три
        // ступени кнопок; владелец посмотрел и выбрал одну. Старшинство
        // несут ДРУГИЕ признаки, и их два: кнопка шире (52 против 44) и глиф
        // в ней светлый (fg против dim у двух соседок).
        CodeButton(
            Icons.AutoMirrored.Filled.ArrowForward, "Подключиться по коду",
            width = 52.dp, glyph = 22.dp, tint = Theme.fg,
        ) { val c = code; onCode(""); app.connectByCode(c) }
    }
}

/** Кнопка в строке кода: одна отделка на все три (ступень s2, скругление 10,
 *  высота 44). Различаются они только шириной и краской глифа — этим и несёт
 *  старшинство «перейти».
 *
 *  `label` — подпись ДЛЯ ОЗВУЧКИ: рядом с глифом нет текста, и без неё TalkBack
 *  прочитал бы безымянную кнопку. */
@Composable
private fun CodeButton(
    icon: ImageVector,
    label: String,
    width: Dp = 44.dp,
    glyph: Dp = 18.dp,
    tint: Color = Theme.dim,
    onTap: () -> Unit,
) {
    Box(
        Modifier.size(width = width, height = 44.dp)
            .background(Theme.cardHi, RoundedCornerShape(10.dp))
            .border(1.dp, Theme.hairline, RoundedCornerShape(10.dp))
            .pressable(onTap = onTap),
        contentAlignment = Alignment.Center,
    ) {
        Icon(icon, label, Modifier.size(glyph), tint = tint)
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

/**
 * СВЕЧЕНИЕ ПОДКЛЮЧЁННОЙ КАРТОЧКИ: МЯГКОЕ СИЯНИЕ НАРУЖУ, А НЕ ВТОРАЯ ОБВОДКА.
 *
 * НЕ ПАЧКАЕТ КАРТОЧКУ: каждая ступень — не заливка, а КОЛЬЦО шириной в одну
 * полосу спада, то есть узкий поясок вокруг контура. Внутри контура не кладётся
 * ни точки. Compose обводит по СЕРЕДИНЕ линии, поэтому прямоугольник раздут на
 * середину полосы, а ширина линии равна полосе: концы приходятся ровно на её
 * края.
 *
 * МЕХАНИЗМ — ЛЕСЕНКА РАЗДУТЫХ КОНТУРОВ, ТЕМ ЖЕ ПРИЁМОМ, ЧТО И `floatHalo`.
 * Смещение контура наружу на d превращает скругление R в R + d, то есть раздутый
 * прямоугольник — ТОЧНАЯ линия равного расстояния до контура: одинаковая сверху,
 * снизу, сбоку и в вырезах углов.
 *
 * РОДНАЯ ТЕНЬ ЗДЕСЬ НЕ ГОДИТСЯ, И ЭТО ЗАМЕР, А НЕ ВКУС. `elevation` у Compose,
 * `radius` у SwiftUI и `drop-shadow-blur` у Slint — РАЗНЫЕ величины: на тени три
 * одинаковых числа дали вылет 13.3 pt против 8.5 px. Плюс `Modifier.blur` тут
 * бесполезен вдвойне — он появился в API 31, а minSdk 24, и на старых
 * устройствах молча не делает ничего. Лесенка от единиц оболочки не зависит
 * вовсе.
 *
 * ПОЛОСЫ НЕ НАКЛАДЫВАЮТСЯ, И ЭТО ГЛАВНОЕ. Каждая точка красится РОВНО ОДИН РАЗ,
 * сразу нужной непрозрачностью `peak·(1 − d/reach)`. Первый заход складывал
 * двадцать четыре полупрозрачных кольца друг на друга — математически то же
 * самое, а на экране НЕТ: у каждого кольца выходила альфа 0.009, это 2.3 из 255,
 * и Skia округляла её вверх там, где Slint и SwiftUI округляли вниз. Замер: у
 * контура 63 по зелёному ЗДЕСЬ против 57 в окне и на iPhone — расхождение 12%,
 * ровно цена одной единицы округления, помноженной на двадцать четыре
 * наложения. Одно наложение вместо двадцати четырёх убирает её целиком:
 * одиночное смешивание все три оболочки считают одинаково (проверено на кромке
 * `hairline` — везде до единицы одно и то же).
 *
 * СТЫКОВ МЕЖДУ ПОЛОСАМИ НЕ ВИДНО, И ЭТО НЕ ВЕЗЕНИЕ: сглаживание делит
 * пограничную точку между соседними полосами по покрытию, а покрытия дают в
 * сумме единицу — выходит ровно та же альфа, что и посередине между ними.
 *
 * ПОЧЕМУ СВЕТ МОЖНО ТАМ, ГДЕ ТЕНЬ БЫЛА НЕЛЬЗЯ. Страница `#08090B` — 8/255, до
 * чистого чёрного восемь единиц: тёмному пятну падать некуда. У света ход в
 * двадцать семь раз больше — мята по зелёному каналу 224 против 9 у фона.
 */
private fun DrawScope.cardGlow(radius: Float) {
    // Полос в спаде. 24 на вылет 12 — по полточки на полосу, меньше пикселя на
    // любом экране, поэтому лесенки не видно: перепад альфы на полосу (0.22/24)
    // даёт по зелёному каналу две единицы из 255.
    val n = 24
    val band = Theme.glowReach.toPx() / n
    for (i in 0 until n) {
        // Середина полосы: по ней считается и её место, и её непрозрачность.
        val mid = band * (i + 0.5f)
        drawRoundRect(
            color = Theme.accent.copy(alpha = Theme.glowPeak * (1f - (i + 0.5f) / n)),
            topLeft = Offset(-mid, -mid),
            size = Size(size.width + 2 * mid, size.height + 2 * mid),
            cornerRadius = CornerRadius(radius + mid),
            style = Stroke(width = band),
        )
    }
}

@Composable
fun HostCard(app: AppState, host: Host, reveal: suspend (LayoutCoordinates) -> Unit) {
    var password by remember(host.id) { mutableStateOf("") }
    val expanded = app.expandedId == host.id
    // МЫ СЕЙЧАС В ЭТОЙ СЕТИ. Не «раскрыта» и не «выбрана» — это разные вещи:
    // раскрыта может быть одна карточка, а работает соединение с другой.
    //
    // `vpnState == 2` ОБЯЗАТЕЛЬНО, а не один `connectedTo`: последний ставится
    // оптимистично, ещё на «Подключаюсь» (`AppState.connect`), и метить им
    // карточку значило бы обещать работающую сеть до того, как она встала.
    val live = app.vpnState == 2 && app.connectedTo == host.id
    // РАСКРЫЛИ — ПОКАЗАТЬ КАРТОЧКУ ЦЕЛИКОМ (правило — в `rememberReveal`).
    // Прокрутка сама за выросшей карточкой не идёт: у последней в списке всё,
    // что ниже заголовка, уезжало под нав-бар, и «Подключить» человек не видел
    // вовсе — на снимке владельца кнопку срезало ровно посередине.
    var coords by remember { mutableStateOf<LayoutCoordinates?>(null) }
    var cardSize by remember { mutableStateOf(IntSize.Zero) }
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
        coords?.let { reveal(it) }
    }
    // ВНУТРИ КАРТОЧКИ МЯТЫ НЕТ — ОДНИ СТУПЕНИ.
    //
    // Пробовали и так, и эдак: сперва мятой красили карточку целиком (цвет
    // доставался оболочке, а не содержимому), потом — плитки внутри (семь
    // подкрашенных ячеек читались как «всё это включено»). Осталась чистая
    // лестница: свёрнутая карточка s1 (L* 6.64), раскрытая s2 (9.06), плитки
    // внутри неё s3 (19.31). Все три различимы, и ни одна ничего не обещает.
    // «Раскрыта» читается ступенью и мятной рамкой (edgeSoft), признак не один.
    val bg by animateColorAsState(if (expanded) Theme.cardHi else Theme.card, tween(200), label = "cardBg")
    // Заливка плиток внутри: ЧИСТАЯ СТУПЕНЬ s3, БЕЗ ПОДКРАСКИ.
    //
    // Мята с плиток снята: в раскрытой карточке их семь, и семь подкрашенных
    // ячеек читались как «всё это включено», хотя это просто цифры о хосте.
    // Ступень s3 для того и заведена — она отличима от своей подложки (L* 10.25
    // над s2 раскрытой карточки), но остаётся нейтральной. Та же ступень лежит
    // под плитками панели состояния, так что по оттенку они одно и то же.
    val tileFill = Theme.tile
    // ── «ПОДКЛЮЧЕНО» ГОВОРИТ СВЕЧЕНИЕ ВОКРУГ, А НЕ КРОМКА И НЕ ЗАЛИВКА ──
    //
    // Заливка уже занята плотно: ступень отвечает на «раскрыта ли», подкраска
    // (picked) — на «выбрано ли». Кромка занята тоже: мятая edgeSoft означает
    // «раскрыта». Подключён при этом может быть ОДИН хост, а раскрыт совсем
    // ДРУГОЙ, и увидеть надо оба сразу.
    //
    // ЗДЕСЬ БЫЛА ОБВОДКА — мята в полную силу и вдвое толще (2dp против 1dp).
    // Владелец посмотрел и сказал: «всмысле обводка а не свечение?» — и он прав:
    // обводка спорила с кромкой «раскрыта» тем же языком, только громче.
    // Свечение живёт СНАРУЖИ карточки, где не занято ничего (см. cardGlow).
    val stroke by animateColorAsState(
        if (expanded) Theme.edgeSoft() else Theme.hairline,
        tween(200), label = "cardStroke",
    )

    Column(
        Modifier
            .fillMaxWidth()
            // Замер и место — САМОЙ ВНЕШНЕЙ коробкой карточки: ниже по цепочке
            // идут заливка, рамка и отступы, и мерить надо то, что человек видит
            // целиком, а не начинку.
            .onGloballyPositioned { coords = it }
            .onSizeChanged { cardSize = it }
            // СВЕЧЕНИЕ. `drawBehind` читает размер в ФАЗЕ РИСОВАНИЯ, а не заводит
            // слежение за раскладкой: за размерами карточки здесь следит ровно
            // один `onSizeChanged` выше, и второго наблюдателя свечение не
            // добавляет. Появляется и пропадает разом с чипом «Подключено» —
            // это одна и та же весть.
            .drawBehind { if (live) cardGlow(16.dp.toPx()) }
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
                    // Чип «Подключено» — В СТРОКЕ ИМЕНИ, а не отдельной полосой:
                    // он говорит про сам хост, и читать его надо там же, где
                    // читают имя. Ширина у чипа фиксированная, у имени сжимаемая
                    // (weight(1f, fill = false)) — место уступает имя.
                    if (live) StateChip("Подключено", Theme.accent)
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
                    // `pressable`, как все прочие кнопки приложения: голый
                    // `clickable` брал `LocalIndication` — чёрную заливку 30%
                    // поверх мятной подкраски, и одна эта кнопка мигала не так.
                    .pressable { app.doUpdate() }
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
