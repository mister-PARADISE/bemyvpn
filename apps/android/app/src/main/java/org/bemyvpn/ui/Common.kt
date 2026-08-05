package org.bemyvpn.ui

import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.EaseOut
import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.VectorConverter
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsPressedAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.HelpOutline
import androidx.compose.material.icons.filled.Autorenew
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material.icons.filled.RemoveModerator
import androidx.compose.material.icons.filled.Security
import androidx.compose.material.icons.filled.SignalWifiOff
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.composed
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.BlendMode
import androidx.compose.ui.graphics.CompositingStrategy
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.graphics.PathFillType
import androidx.compose.ui.graphics.vector.path
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.delay
import org.bemyvpn.Haptics
import org.bemyvpn.Host
import org.bemyvpn.Ping
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.scale
import androidx.compose.ui.layout.layout
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.Dp
import androidx.compose.material.icons.filled.QrCode2
import org.bemyvpn.Theme

// ── Значки протоколов (аналоги SF Symbols из iOS) ─────────────────────────────
// Семейство ЩИТА — уровень защиты трафика: есть / замаскирована / нет.
//
// ВЫБОР ПО УРОВНЮ ЗАЩИТЫ (номера view::Protection, их считает мост), А НЕ ПО
// ИМЕНИ ПРОТОКОЛА. Список имён здесь был второй копией справочника: пустое имя
// приходилось помнить отдельно и в подписи, и в значке, а «какие имена значат
// шифрование» решалось в двух местах сразу.
fun protoIcon(protection: Int): ImageVector = when (protection) {
    0 -> Icons.Filled.Security              // защищено (щит с замком)
    1 -> MaskIcon                           // прячет сам факт VPN (маска, как iOS theatermasks)
    2 -> Icons.Filled.RemoveModerator       // защиты нет (щит перечёркнут)
    else -> Icons.AutoMirrored.Filled.HelpOutline
}

/** Маскарадная маска — значок уровня «прячет факт VPN» (view::Protection = 1).
 *  Аккуратный вектор с прорезями-глазами (evenodd), чистый на любом размере.
 *  Аналог iOS theatermasks.fill, но без «мусора» на мелких размерах. */
val MaskIcon: ImageVector by lazy {
    ImageVector.Builder(name = "Mask", defaultWidth = 24.dp, defaultHeight = 24.dp, viewportWidth = 24f, viewportHeight = 24f).apply {
        path(fill = SolidColor(androidx.compose.ui.graphics.Color.White), pathFillType = PathFillType.EvenOdd) {
            // Контур маски (широкая полоса на глаза, чуть опускается к центру).
            moveTo(3f, 8f)
            curveTo(6f, 6.4f, 9f, 6.4f, 12f, 8f)
            curveTo(15f, 6.4f, 18f, 6.4f, 21f, 8f)
            curveTo(21.6f, 11.2f, 20f, 14.6f, 16.2f, 14.6f)
            curveTo(14.1f, 14.6f, 12.9f, 13.1f, 12f, 11.7f)
            curveTo(11.1f, 13.1f, 9.9f, 14.6f, 7.8f, 14.6f)
            curveTo(4f, 14.6f, 2.4f, 11.2f, 3f, 8f)
            close()
            // Левый глаз (прорезь).
            moveTo(6.2f, 9.9f)
            arcToRelative(1.5f, 1.5f, 0f, isMoreThanHalf = true, isPositiveArc = true, 3f, 0f)
            arcToRelative(1.5f, 1.5f, 0f, isMoreThanHalf = true, isPositiveArc = true, -3f, 0f)
            close()
            // Правый глаз (прорезь).
            moveTo(14.8f, 9.9f)
            arcToRelative(1.5f, 1.5f, 0f, isMoreThanHalf = true, isPositiveArc = true, 3f, 0f)
            arcToRelative(1.5f, 1.5f, 0f, isMoreThanHalf = true, isPositiveArc = true, -3f, 0f)
            close()
        }
    }.build()
}

// ── Нажатие: лёгкое «вдавливание» под пальцем, без Material-рипла (как iOS) ───
fun Modifier.pressable(enabled: Boolean = true, onTap: () -> Unit): Modifier = composed {
    val interaction = remember { MutableInteractionSource() }
    val pressed by interaction.collectIsPressedAsState()
    val scale by animateFloatAsState(if (pressed) 0.97f else 1f, tween(120), label = "press")
    this
        .graphicsLayer { scaleX = scale; scaleY = scale }
        .clickable(interactionSource = interaction, indication = null, enabled = enabled, onClick = onTap)
}

/** Тап без визуального отклика (строки, чипы). */
fun Modifier.tappable(onTap: () -> Unit): Modifier = composed {
    clickable(interactionSource = remember { MutableInteractionSource() }, indication = null, onClick = onTap)
}

/**
 * ДИСК СОСТОЯНИЯ — ОДНА ДЕТАЛЬ НА ВСЕ РАЗМЕРЫ.
 *
 * Заливка `discFill` плюс кольцо `discRing` были написаны девятью копиями с
 * диаметрами 38/72/74/84/88 — по одной на каждое место, где диск понадобился.
 *
 * Размер здесь ЕДИНСТВЕННЫЙ параметр — `d`. Ни своей заливки, ни толщины кольца,
 * ни тени: как только у диска появится второй способ выглядеть, начнётся вторая
 * копия. Значок кладёт вызывающий.
 */
@Composable
fun StateDisc(
    tint: Color,
    d: Dp = 38.dp,
    /** Расходящаяся волна — «идёт процесс». Всегда 1200мс и ×1.28. */
    pulsing: Boolean = false,
    icon: @Composable () -> Unit,
) {
    Box(Modifier.size(d), contentAlignment = Alignment.Center) {
        if (pulsing) {
            val inf = rememberInfiniteTransition(label = "disc")
            val p by inf.animateFloat(
                0f, 1f,
                infiniteRepeatable(tween(1200, easing = EaseOut), RepeatMode.Restart),
                label = "discP",
            )
            // scale+alpha одним слоем: `Modifier.scale` сюда не позвать — имя
            // `scale` в этом файле уже занято рисовальным (см. `floatHalo`), а
            // оба модификатора всё равно раскладываются ровно в этот вызов.
            Box(
                Modifier
                    .size(d)
                    .graphicsLayer {
                        scaleX = 1f + 0.28f * p
                        scaleY = 1f + 0.28f * p
                        alpha = 0.7f * (1f - p)
                    }
                    .border(2.dp, tint.copy(alpha = 0.5f), CircleShape),
            )
        }
        Box(
            Modifier
                .size(d)
                .background(Theme.discFill(tint), CircleShape)
                .border(1.dp, Theme.discRing(tint), CircleShape),
            contentAlignment = Alignment.Center,
        ) { icon() }
    }
}

/**
 * Круг героя: диск состояния в геройском размере плюс волна, пока идёт процесс
 * (проверка/пробитие). Общий для всех трёх вкладок — оттого и живёт здесь, а не
 * в файле одной из них, как раньше.
 */
@Composable
fun HeroCircle(tint: Color, icon: ImageVector, pulsing: Boolean) {
    // Контейнер 108dp вмещает расходящееся кольцо ЦЕЛИКОМ — иначе герой-Column с
    // animateContentSize обрезает верх кольца («заезжает под блок»).
    Box(Modifier.size(108.dp), contentAlignment = Alignment.Center) {
        // 72, а не 84: панель прижата и висит на экране постоянно — каждый лишний
        // десяток пикселей забирается у содержимого.
        StateDisc(tint, 72.dp, pulsing) { Icon(icon, null, Modifier.size(31.dp), tint = tint) }
    }
}

// ── Секундный тикер (аналог TimelineView .periodic 1с) ───────────────────────
@Composable
fun rememberSecondTick(): Long {
    var tick by remember { mutableLongStateOf(System.currentTimeMillis()) }
    LaunchedEffect(Unit) { while (true) { delay(1000); tick = System.currentTimeMillis() } }
    return tick
}

// ── Плитки ───────────────────────────────────────────────────────────────────

/**
 * Фон плитки: единый для всех — разница только в содержимом, не в оформлении.
 *
 * `fill` — нейтральная ступень s3, ОДНА И ТА ЖЕ у плиток панели состояния и у
 * плиток внутри раскрытой карточки хоста. Ручка оставлена затем, что раскрытая
 * карточка задаёт заливку одним местом на всю сетку (см. `tileFill` в
 * `VpnTab.kt`), а не семью вызовами по отдельности.
 */
fun Modifier.tileBackground(accent: Color? = null, fill: Color = Theme.tile): Modifier = this
    .background(fill, RoundedCornerShape(12.dp))
    .border(1.dp, accent ?: Theme.hairline, RoundedCornerShape(12.dp))

/** Общая «шапка» плитки — подпись мелко сверху, значение снизу. */
@Composable
fun TileBody(
    label: String,
    value: String,
    symbol: ImageVector? = null,
    valueColor: Color = Theme.fg,
    mono: Boolean = false,
    trailing: @Composable () -> Unit = {},
) {
    // Пара «подпись + значение» центрируется в ячейке фиксированной высоты.
    // Раньше высоту задавали отступы, а сумма строк с их межстрочными
    // интервалами её перекрывала — значение выдавливало к нижнему краю, и
    // казалось, что оно проваливается.
    Column(
        Modifier.fillMaxWidth().heightIn(min = 52.dp).padding(horizontal = 11.dp, vertical = 8.dp),
        verticalArrangement = Arrangement.spacedBy(3.dp, Alignment.CenterVertically),
    ) {
        Text(label, color = Theme.dim, fontSize = 10.sp, fontWeight = FontWeight.Black, letterSpacing = 0.7.sp)
        // Значение и значок — ПО ЦЕНТРУ ячейки. Раньше symbol вставал слева от
        // текста, а trailing уезжал вправо за weight(1f), и значки в соседних
        // плитках оказывались по разные стороны — взгляд цеплялся за разнобой.
        // Заголовок остаётся слева: он подпись к ячейке, а не содержимое.
        Row(
            Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(5.dp, Alignment.CenterHorizontally),
        ) {
            if (symbol != null) Icon(symbol, null, Modifier.size(14.dp), tint = valueColor)
            if (value.isNotEmpty()) {
                Text(
                    value, color = valueColor, fontSize = 13.5.sp, fontWeight = FontWeight.SemiBold,
                    fontFamily = if (mono) FontFamily.Monospace else null,
                    maxLines = 1, overflow = TextOverflow.Ellipsis,
                    // ЦИФРЫ ТАБУЛЯРНЫЕ (tnum) — аналог monospacedDigit() на iOS.
                    // Пинг обновляется раз в секунду, а у пропорциональных цифр
                    // «24 мс» и «38 мс» ширина разная — строка здесь
                    // центрируется, и значение дёргалось бы вбок на каждом
                    // замере. Явные параметры выше перекрывают style, поэтому от
                    // него берётся только эта настройка.
                    style = TextStyle(fontFeatureSettings = "tnum"),
                )
            }
            trailing()
        }
    }
}

/** Плитка-факт. */
@Composable
fun StatTile(
    label: String,
    value: String,
    modifier: Modifier = Modifier,
    symbol: ImageVector? = null,
    tint: Color = Theme.fg,
    fill: Color = Theme.tile,
) {
    Box(modifier.tileBackground(fill = fill)) { TileBody(label, value, symbol, tint) }
}

/** Плитка со значением, которое копируется тапом (код, IP). */
@Composable
fun CopyTile(label: String, value: String, modifier: Modifier = Modifier, fill: Color = Theme.tile) {
    val clipboard = LocalClipboardManager.current
    val ctx = LocalContext.current
    var copied by remember { mutableStateOf(false) }
    val empty = value.isEmpty() || value == "—"
    LaunchedEffect(copied) { if (copied) { delay(Theme.COPIED_MS); copied = false } }
    Box(modifier.tileBackground(if (copied) Theme.edgeDone() else null, fill).tappable {
        if (!empty) { clipboard.setText(AnnotatedString(value)); Haptics.tap(ctx); copied = true }
    }) {
        TileBody(
            label, if (copied) "Скопировано" else value,
            valueColor = if (copied) Theme.accent else Theme.fg, mono = !copied,
        ) {
            if (!empty) Icon(
                if (copied) Icons.Filled.Check else Icons.Filled.ContentCopy, null,
                Modifier.size(13.dp), tint = Theme.accent,
            )
        }
    }
}

/**
 * Плитка отклика — ОДНА НА ОБА ПИНГА: до хоста и до координатора.
 *
 * Их было две, со своими линейками, и меньшее число выходило тревожнее большего:
 * 137 мс до хоста краснело, 162 мс до координатора числилось «хорошо». Теперь
 * подпись и уровень тревоги приходят ГОТОВОЙ ПАРОЙ (`Ping` от моста) — экран
 * больше не разбирает подпись обратно в число, чтобы выбрать цвет.
 *
 *   • идёт первый замер — КРУГОВАЯ СТРЕЛКА ВРАЩАЕТСЯ: движение честно говорит
 *     «сейчас меряю», в отличие от многоточия, которое просто стоит и молчит;
 *   • ответа нет — перечёркнутая антенна: у «нет отклика» отдельный знак, а не
 *     прочерк, который легко принять за «данных нет»;
 *   • число есть — просто цифра, без анимации: крутить что-то поверх готового
 *     значения значит намекать, что оно ещё не готово.
 */
@Composable
fun PingTile(ping: Ping, modifier: Modifier = Modifier, fill: Color = Theme.tile) {
    when {
        // Ожидание первого замера — состояние ЭКРАНА, а не справочника: у моста
        // на него ответа нет (см. `Ping.measuring`), зато есть эта анимация.
        ping == Ping.measuring -> {
            val t = rememberInfiniteTransition(label = "ping")
            val angle by t.animateFloat(
                0f, 360f,
                infiniteRepeatable(tween(900, easing = LinearEasing), RepeatMode.Restart),
                label = "pingSpin",
            )
            // Через trailing у TileBody, чтобы не менять общий StatTile ради
            // одного случая: вращение нужно только здесь.
            Box(modifier.tileBackground(fill = fill)) {
                TileBody("ПИНГ", "") {
                    Icon(
                        Icons.Filled.Autorenew, null,
                        Modifier.size(14.dp).rotate(angle),
                        tint = Theme.accent,
                    )
                }
            }
        }
        // Только знак, без слова: перечёркнутая антенна говорит сама, а подпись
        // рядом с ней — это то же самое ещё раз.
        ping.alarm == ALARM_MUTED ->
            StatTile("ПИНГ", "", modifier, symbol = Icons.Filled.SignalWifiOff, tint = Theme.dim, fill = fill)
        else -> StatTile("ПИНГ", ping.text, modifier, tint = alarmTint(ping.alarm), fill = fill)
    }
}

/** «Ответа нет» — вариант view::Alarm, его номер приходит через мост. */
private const val ALARM_MUTED = 3

/**
 * Уровень тревоги (view::Alarm) → цвет. ПОРОГИ СЧИТАЕТ СПРАВОЧНИК, экран только
 * подбирает краску: наборов цветов у оболочек столько же, сколько оболочек.
 *
 * АКЦЕНТА ЗДЕСЬ НЕТ ВОВСЕ: мята в этом приложении значит «работает», а не
 * «быстро». Норма молчит — обычный цвет текста.
 */
private fun alarmTint(alarm: Int): Color = when (alarm) {
    1 -> Theme.amber
    2 -> Theme.red
    ALARM_MUTED -> Theme.dim
    else -> Theme.fg
}

/**
 * Связь с координатором для моста: 1 — да, 0 — нет, −1 — ещё не знаем.
 *
 * Третье состояние отдельно от «нет связи» не для красоты: приложение только
 * открылось, ответа ещё не было, и обвинять сеть не в чем.
 */
fun triOnline(online: Boolean?): Int = when (online) {
    true -> 1
    false -> 0
    null -> -1
}

// ── Кнопки ───────────────────────────────────────────────────────────────────

/** Крупная кнопка «скопировать» под QR-кодом. Спокойная, как и все остальные:
 *  подкраска + рамка + цветной текст. Сплошной градиент был единственным ярким
 *  пятном на почти пустом экране и перетягивал взгляд с самого кода. */
@Composable
fun BigCopyButton(value: String, modifier: Modifier = Modifier) {
    val clipboard = LocalClipboardManager.current
    val ctx = LocalContext.current
    var copied by remember { mutableStateOf(false) }
    LaunchedEffect(copied) { if (copied) { delay(Theme.COPIED_MS); copied = false } }
    val hue = Theme.accent
    Row(
        modifier
            .background(Theme.picked(hue), RoundedCornerShape(14.dp))
            .border(1.dp, Theme.edge(hue, bright = copied), RoundedCornerShape(14.dp))
            .pressable {
                if (value.isNotEmpty()) { clipboard.setText(AnnotatedString(value)); Haptics.success(ctx); copied = true }
            }.padding(vertical = 15.dp),
        horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.CenterHorizontally),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(if (copied) Icons.Filled.Check else Icons.Filled.ContentCopy, null, Modifier.size(17.dp), tint = hue)
        Text(if (copied) "Скопировано" else "Скопировать код", color = hue, fontSize = 16.sp, fontWeight = FontWeight.Bold)
    }
}

/** Главная кнопка — ОДНА НА ВСЁ ПРИЛОЖЕНИЕ, спокойная: подкраска + рамка +
 *  цветной текст, тот же приём, что у ShareButton и у действующей ячейки
 *  нав-бара.
 *
 *  Градиентного близнеца здесь больше нет. Сплошная синяя плашка во всю ширину
 *  кричала громче всего на экране и перетягивала внимание с того, ради чего на
 *  экран смотрят: в карточке хоста — с его же цифр, на вкладке «Сервер» — с
 *  адреса и состояния связи. Один стиль на все кнопки — ещё и способ не решать
 *  каждый раз заново, какая из них «главнее». */
@Composable
fun CalmButton(title: String, modifier: Modifier = Modifier, enabled: Boolean = true, onTap: () -> Unit) {
    Box(
        modifier
            .fillMaxWidth()
            .alpha(if (enabled) 1f else 0.5f)
            // Та же подкраска, что у выбранного чипа: кнопка тоже лежит на
            // странице, и прозрачным акцентом она смешивалась с почти чёрным
            // фоном — выходила ТЕМНЕЕ соседнего поля ввода (13.6 против 15.5 по
            // L*) и читалась вдавленной.
            .background(Theme.picked(), RoundedCornerShape(14.dp))
            .border(1.dp, Theme.edge(), RoundedCornerShape(14.dp))
            .pressable(enabled = enabled, onTap = onTap)
            .padding(vertical = 15.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(title, color = Theme.accent, fontSize = 16.sp, fontWeight = FontWeight.Bold)
    }
}

// ── Карточка и подписи ───────────────────────────────────────────────────────

@Composable
fun SectionLabel(t: String) {
    Text(
        t, color = Theme.dim, fontSize = 13.sp, fontWeight = FontWeight.Bold,
        modifier = Modifier.fillMaxWidth().padding(top = 6.dp),
    )
}

// ── Поле ввода в стиле iOS (без Material-декора) ─────────────────────────────
@Composable
fun BmvTextField(
    value: String,
    onValueChange: (String) -> Unit,
    placeholder: String,
    modifier: Modifier = Modifier,
    keyboardOptions: KeyboardOptions = KeyboardOptions.Default,
    secure: Boolean = false,
    mono: Boolean = false,
    boxed: Boolean = true,
) {
    val style = TextStyle(
        color = Theme.fg, fontSize = 16.sp,
        fontFamily = if (mono) FontFamily.Monospace else null,
    )
    val body: @Composable () -> Unit = {
        BasicTextField(
            value = value,
            onValueChange = onValueChange,
            textStyle = style,
            singleLine = true,
            keyboardOptions = keyboardOptions,
            visualTransformation = if (secure) PasswordVisualTransformation() else VisualTransformation.None,
            cursorBrush = SolidColor(Theme.accent),
            modifier = Modifier.fillMaxWidth(),
            decorationBox = { inner ->
                Box {
                    if (value.isEmpty()) Text(placeholder, color = Theme.dim, fontSize = 16.sp, fontFamily = style.fontFamily)
                    inner()
                }
            },
        )
    }
    if (boxed) {
        Box(modifier.fillMaxWidth().clip(RoundedCornerShape(14.dp)).background(Theme.card).padding(14.dp)) { body() }
    } else {
        Box(modifier) { body() }
    }
}

// ── Прижатая панель и её содержимое ─────────────────────────────────────────

/**
 * Вылет гашения от контура парящего блока наружу. Отступы прокрутки считаются ОТ
 * НЕГО — покоящееся содержимое не должно попадать в зону гашения, иначе оно
 * выглядит подтенённым на пустом месте.
 */
val veilWidth = 18.dp   // было 24, укорочено на четверть по просьбе владельца

/**
 * Насколько ореол уходит ЗА блок, к ближнему краю экрана. Блок прижат к краю, но
 * не вплотную; в этот просвет содержимое пролезало и обрывалось о край экрана.
 * Ореол закрывает просвет ТЕМ ЖЕ силуэтом — одна форма, без второго механизма и
 * без стыка. С запасом на любую высоту системной панели: за экраном лишний фон
 * ничего не стоит, а не хватило бы — вышла бы жёсткая кромка поперёк экрана.
 */
private val haloBack = 200.dp

/**
 * Сплошная часть ореола: до неё гашение ПОЛНОЕ, дальше сходит на нет к `veilWidth`.
 *
 * ЧИСЛО НЕ ИЗ ВКУСА, А ИЗ ГЕОМЕТРИИ ВЫРЕЗА, И ТЕПЕРЬ ОНО ЕЙ И СЧИТАЕТСЯ. Самая
 * дальняя от контура точка выреза скруглённого угла — сам угол габаритной
 * коробки, и он отстоит от дуги ровно на R·(√2−1): 9.11 при радиусе панели 22 и
 * 11.60 при радиусе бара 28. Гашение, которое начинает спадать прямо от контура,
 * в этой точке даёт уже половину яркости — вырез принципиально не вычистить,
 * пока сплошная часть не перекрывает его ЦЕЛИКОМ.
 *
 * Раньше здесь стояло плоское 12 — потолок из двух блоков, взятый с запасом.
 * Теперь считается по своему радиусу: панели хватает 9.61, бару нужно 12.10.
 * Панель от этого гасит на 20% короче в полную силу — ровно та «менее
 * интенсивная» часть, которую просили; бару уступить нечего, его 11.60 и есть
 * физический предел. Полпункта сверху — запас на округление до пикселя.
 */
private fun haloSolid(radius: Dp) = radius * 0.4142136f + 0.5.dp

/**
 * ГАШЕНИЕ — ОРЕОЛ ПО КОНТУРУ БЛОКА, А НЕ ПОЛОСА РЯДОМ С НИМ. Вешается на ТОТ ЖЕ
 * элемент, что и `floatSurface`, но РАНЬШЕ него в цепочке — тогда рисуется под
 * ним.
 *
 * Полоса гасила только «до» и «после» блока: сбоку от карточки содержимое шло в
 * полную яркость, а в вырезах скруглённых углов пролезало наружу и обрывалось
 * ножом (замер: 73 точки текста в левом верхнем вырезе панели). Ореол считает
 * расстояние ДО КОНТУРА, одинаково сверху, снизу, сбоку и в вырезах углов.
 *
 * СОБРАН ИЗ ГРАДИЕНТОВ, А НЕ ИЗ РАЗМЫТИЯ, и это не выбор вкуса: `Modifier.blur`
 * появился только в API 31, а minSdk здесь 24 — на старых устройствах он молча
 * ничего не делает, и вместо гашения остался бы жёсткий силуэт. Градиенты дают
 * ТОЧНЫЙ линейный ход и работают везде.
 *
 * Кликов не перехватывает: рисование в Compose жестов не ловит вовсе.
 *
 * На коротком списке невидим по построению: это фон страницы, растворяющийся в
 * фоне страницы, — тенью посреди пустоты он стать не может.
 */
fun Modifier.floatHalo(radius: Dp, up: Boolean = false): Modifier = this.drawBehind {
    // Нав-бар — тот же ореол, отражённый по вертикали: форма симметрична, и
    // вторая система координат ради этого не нужна.
    scale(1f, if (up) -1f else 1f, pivot = Offset(size.width / 2f, size.height / 2f)) {
        panelHalo(radius.toPx(), veilWidth.toPx(), haloSolid(radius).toPx(), haloBack.toPx())
    }
}

/** Ореол блока, прижатого СВЕРХУ: гашение по нижней кромке, бокам и низким углам. */
private fun DrawScope.panelHalo(r: Float, v: Float, s: Float, back: Float) {
    val w = size.width
    val h = size.height
    val bg = Theme.bg
    // Прозрачный конец — ФОН С НУЛЕВОЙ АЛЬФОЙ, а не Color.Transparent: тот
    // прозрачно-ЧЁРНЫЙ, и на непремультиплицированной интерполяции середина
    // градиента вышла бы темнее обоих концов — тёмной полосой поперёк экрана.
    val clear = bg.copy(alpha = 0f)
    // Высота прямой части: ниже начинаются дуги углов, там считает радиальный.
    val straight = back + h - r
    // 1. За блоком, к краю экрана, — сплошной фон во всю его ширину.
    drawRect(bg, Offset(0f, -back), Size(w, straight))
    // Ход одинаков на всех кромках: сплошное до s, дальше линейно на нет к v.
    val out = arrayOf(0f to bg, s / v to bg, 1f to clear)
    val inn = arrayOf(0f to clear, 1f - s / v to bg, 1f to bg)
    // 2. Бока: гаснут наружу по горизонтали.
    drawRect(
        Brush.horizontalGradient(*inn, startX = -v, endX = 0f),
        Offset(-v, -back), Size(v, straight),
    )
    drawRect(
        Brush.horizontalGradient(*out, startX = w, endX = w + v),
        Offset(w, -back), Size(v, straight),
    )
    // 3. Кромка подъезда: гаснет вниз. Только между дугами — по краям её
    //    продолжают углы, иначе на стыке сложились бы два гашения и вышло бы
    //    тёмное пятно.
    drawRect(
        Brush.verticalGradient(*out, startY = h, endY = h + v),
        Offset(r, h), Size(w - 2 * r, v),
    )
    // 4. ВЫРЕЗЫ УГЛОВ. Снаружи дуги расстояние до контура — радиальное от центра
    //    дуги, поэтому и гашение здесь радиальное: сплошное до r + s, дальше на
    //    нет к r + v — тот же ход, что и на прямых кромках. Ровно из-за этого
    //    куска текст больше не торчит из-под скруглений.
    val stops = arrayOf(0f to bg, (r + s) / (r + v) to bg, 1f to clear)
    drawRect(
        Brush.radialGradient(*stops, center = Offset(r, h - r), radius = r + v),
        Offset(-v, h - r), Size(r + v, r + v),
    )
    drawRect(
        Brush.radialGradient(*stops, center = Offset(w - r, h - r), radius = r + v),
        Offset(w - r, h - r), Size(r + v, r + v),
    )
}

/**
 * Отделка парящего слоя: заливка + тихая рамка. Одна на панель и нав-бар.
 *
 * ЗАЛИВКА — Theme.float, ОДНА на панель и нав-бар; своей константой здесь она
 * молча осталась бы синей, когда вся лестница ушла в уголь. Непрозрачная:
 * прежние 0.97 не покупали ничего — размытия под блоком нет и быть не может
 * (`Modifier.blur` размывает сам элемент, а не фон под ним, а перерисовывать фон
 * в отдельный слой каждый кадр прокрутки — расход батареи ради эффекта, почти
 * невидимого в тёмной теме), зато сквозь панель в прокрутке читался текст списка.
 *
 * ТЕНИ ЗДЕСЬ НЕТ, И ЭТО ЗАМЕР, А НЕ ЛЕНЬ. `Modifier.shadow(22.dp)` на почти
 * чёрном фоне темнил подложку под панелью на ОДНУ единицу из 255 — чёрной тени
 * на почти чёрном фоне физически быть не может. Штатный механизм Android вдобавок
 * не умеет направить тень вверх (нав-бар прижат снизу, и тень уезжала под него,
 * в полосу, на которую никто не смотрит).
 *
 * КРОМКИ СВЕТА 1px СВЕРХУ БОЛЬШЕ НЕТ. Она была подпоркой под тёмную палитру: на
 * прежних поверхностях блок отличался от фона на 6.4 по L*, край приходилось
 * обводить руками, и полоска бросалась в глаза. Теперь высоту держит ОДИН
 * носитель — непрозрачная заливка светлее страницы на 11.5 по L*.
 */
fun Modifier.floatSurface(radius: Dp, stroke: Color): Modifier {
    val shape = RoundedCornerShape(radius)
    return this
        .background(Theme.float, shape)
        .border(1.dp, stroke, shape)
}

/**
 * ПАРЯЩАЯ ПАНЕЛЬ поверх прокрутки: панель кладётся НАД содержимым, а прокрутка
 * занимает всю высоту и получает верхний отступ ровно в высоту панели.
 * Содержимое проходит ПОД панелью и видно в зазорах вокруг её карточки.
 *
 * Соседом сверху в `Column` панель занимала собственную непрозрачную полосу во
 * всю ширину: список упирался в её низ, а по бокам от скруглённой карточки
 * стояла пустая полоса цвета страницы — тот самый «квадрат с фоном как у
 * основы», на который жаловались.
 *
 * Высота панели МЕРЯЕТСЯ, а не задаётся числом: она меняется вместе с
 * состоянием (при связи — цифры, без связи — крупный круг).
 */
@Composable
fun FloatingPanelLayout(
    panel: @Composable () -> Unit,
    content: @Composable (topPad: Dp) -> Unit,
) {
    var panelH by remember { mutableStateOf(0.dp) }
    val density = LocalDensity.current
    Box(Modifier.fillMaxSize()) {
        content(panelH)
        // Гашение живёт НА САМОЙ карточке панели (floatHalo в PinnedPanel), а не
        // отдельным слоем здесь: оно свойство блока, а не полоса рядом с ним.
        // Порядок в Box и есть порядок отрисовки — панель поверх прокрутки.
        Box(Modifier.onSizeChanged { panelH = with(density) { it.height.toDp() } }) { panel() }
    }
}

// ── Плавная высота панели ────────────────────────────────────────────────────

/** Сколько едет высота панели. Столько же на iOS и в окне. */
private const val PANEL_H_MS = 170

/**
 * Зона растворения у нижней кромки панели — РОВНО её нижний внутренний отступ.
 * В покое там пусто, поэтому маска в покое не видна: замер низа панели до и
 * после её появления совпал.
 */
private val panelFade = 16.dp

/**
 * ПЛАВНАЯ ВЫСОТА. Содержимое меряется ЦЕЛИКОМ (обычными входными
 * ограничениями), наружу отдаётся едущая высота — то, что пока не поместилось,
 * гасит маска (см. `fadeBottom`), а не нож.
 *
 * `animateContentSize` для этого не годится: он НАЧИНАЕТСЯ с `clipToBounds()`,
 * то есть режет содержимое жёстким краем по едущему размеру. Ровно оттого слово
 * «Старт» и разрезало пополам.
 *
 * `onSizeChanged` стоит ПОСЛЕ `layout` в цепочке, то есть меряет содержимое, а
 * не то, что мы отдали наружу: обратной связи «отдали меньше → содержимое
 * сжалось» не возникает.
 */
private fun Modifier.smoothHeight(): Modifier = composed {
    var natural by remember { mutableIntStateOf(-1) }
    val h = remember { Animatable(0, Int.VectorConverter) }
    LaunchedEffect(natural) {
        if (natural < 0) return@LaunchedEffect
        // Первая высота встаёт мгновенно: ехать от нуля значило бы ронять панель
        // сверху при каждом запуске.
        if (h.value == 0) h.snapTo(natural) else h.animateTo(natural, tween(PANEL_H_MS))
    }
    this
        .layout { measurable, constraints ->
            val p = measurable.measure(constraints)
            layout(p.width, if (h.value == 0) p.height else h.value) { p.place(0, 0) }
        }
        .onSizeChanged { natural = it.height }
}

/**
 * МАСКА ВМЕСТО НОЖА: последние `panelFade` высоты растворяются в прозрачность.
 *
 * Вешается на узел, который ВКЛЮЧАЕТ нижний отступ панели, — тогда зона
 * растворения приходится ровно на этот отступ, где в покое ничего нет. Пока
 * высота едет, строка, ещё не поместившаяся в карточку, не обрывается краем, а
 * проявляется сквозь растворение.
 *
 * `Offscreen` здесь обязателен: без него `DstIn` смешивался бы со всем, что уже
 * нарисовано в родительском слое, а не только с содержимым панели.
 */
private fun Modifier.fadeBottom(): Modifier = this
    .graphicsLayer { compositingStrategy = CompositingStrategy.Offscreen }
    .drawWithContent {
        drawContent()
        val fade = panelFade.toPx()
        drawRect(
            Brush.verticalGradient(
                listOf(Color.Black, Color.Transparent),
                startY = size.height - fade,
                endY = size.height,
            ),
            blendMode = BlendMode.DstIn,
        )
    }

/**
 * Обёртка парящей панели: карточка состояния, лежащая НАД прокруткой
 * (см. FloatingPanelLayout).
 *
 * Содержимое подменяется МГНОВЕННО — гашения начинки здесь нет и не должно
 * быть. Пробовали: панель на время переезда оставалась пустой, и мигание блока
 * во весь верх экрана заметнее того рывка, ради которого всё затевалось. Едет
 * ТОЛЬКО высота, а лишнее у нижней кромки снимает маска (`fadeBottom`).
 */
@Composable
fun PinnedPanel(tint: Color, content: @Composable ColumnScope.() -> Unit) {
    // У ОБЁРТКИ НЕТ НИ ФОНА, НИ ТЕНИ — ровно затем, чтобы сквозь боковые зазоры
    // было видно, как содержимое проходит насквозь. Фон и тень — только у самой
    // карточки, по её скруглённому контуру (floatSurface).
    Box(
        Modifier
            .fillMaxWidth()
            .padding(start = 20.dp, end = 20.dp, top = 20.dp, bottom = 12.dp),
    ) {
        Column(
            Modifier
                .fillMaxWidth()
                // Ореол — РАНЬШЕ отделки: рисуется под карточкой, а не поверх.
                .floatHalo(22.dp)
                // Рамка берёт цвет состояния: на «нет связи» контур красный.
                // 0.30, КАК В ОКНЕ И НА iOS. Здесь стояло 0.45 — расхождение
                // держалось незаметным, пока состояние красилось тёмным синим;
                // на светлом dim «выключено» тот же 0.45 дал ободок #5D636C,
                // вдвое ярче собственной заливки панели, и кольцо стало самым
                // светлым пятном экрана.
                .floatSurface(22.dp, tint.copy(alpha = 0.30f))
                // Глушит тапы по пустым местам карточки: без этого палец
                // проваливался бы на карточку хоста, лежащую ПОД панелью.
                // Прокрутке не мешает — при протяжке жест выигрывает скролл, а
                // кнопки внутри панели перехватывают тап раньше, чем сюда.
                .tappable {}
                // Высота едет ЗДЕСЬ, выше отступов и ниже отделки: карточка,
                // рамка и ореол берут размер отсюда и едут вместе с ней, а
                // `FloatingPanelLayout` меряет эту же коробку — значит и верхний
                // отступ прокрутки едет в ногу, без подскока содержимого.
                // МАСКА — СТРОГО ПЕРЕД `smoothHeight`, И ЭТО НЕ ВКУСОВЩИНА.
                // Модификатор рисования видит размер того, что стоит ПОСЛЕ него:
                // за `smoothHeight` он получил бы натуральную высоту содержимого,
                // растворял бы его собственный низ и не обрезал ничего — на
                // кадрах текст торчал из карточки (5 кадров из 40). Перед ним —
                // видит едущую высоту, ту же, что карточка и ореол.
                .fadeBottom()
                .smoothHeight()
                .padding(vertical = 16.dp, horizontal = 18.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
            content = content,
        )
    }
}

/**
 * Строка состояния для РАБОТАЮЩЕГО режима: значок кружком, название, часы.
 *
 * Диск здесь вчетверо меньше геройского — тот же `StateDisc`, только с другим `d`.
 * Значок никуда не девается и в работе: он опознаёт экран с одного взгляда. Но
 * держать 72dp картинки там, где нужны код и цифры, расточительно: панель прижата
 * к верху и висит на экране постоянно.
 */
@Composable
fun StatusLine(icon: ImageVector, title: String, tint: Color, clock: String? = null) {
    Row(
        Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        StateDisc(tint) { Icon(icon, null, Modifier.size(19.dp), tint = tint) }
        Text(
            title, color = Theme.fg, fontSize = 17.sp, fontWeight = FontWeight.Black,
            maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f),
        )
        if (clock != null) {
            Text(clock, color = tint, fontSize = 14.sp, fontWeight = FontWeight.Bold, fontFamily = FontFamily.Monospace)
        }
    }
}

/**
 * Кнопки «поделиться» — В НИЗУ панели состояния, прямо под кодом.
 *
 * Код сети и QR — то, ради чего эту панель открывают. Оторванные от кода, который
 * копируют, они читались бы как отдельная штука неясно про что.
 */
@Composable
fun ShareButtons(code: String, showQr: (String) -> Unit) {
    val clipboard = LocalClipboardManager.current
    val ctx = LocalContext.current
    var copied by remember { mutableStateOf(false) }
    LaunchedEffect(copied) { if (copied) { delay(Theme.COPIED_MS); copied = false } }
    Row(Modifier.fillMaxWidth().padding(top = 2.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        ShareButton(
            if (copied) Icons.Filled.Check else Icons.Filled.ContentCopy,
            if (copied) "Скопировано" else "Скопировать",
            copied, Modifier.weight(1f),
        ) { clipboard.setText(AnnotatedString(code)); Haptics.tap(ctx); copied = true }
        ShareButton(Icons.Filled.QrCode2, "QR-код", false, Modifier.weight(1f)) { showQr(code) }
    }
}

@Composable
private fun ShareButton(icon: ImageVector, title: String, done: Boolean, modifier: Modifier, tap: () -> Unit) {
    val hue = Theme.accent
    Row(
        modifier
            .height(48.dp)
            // Ступень s3 — та же, что у плиток рядом: кнопка живёт только внутри
            // парящей панели, а панель светлее карточек списка (см. ShareButton
            // в components.slint).
            .background(Theme.tile, RoundedCornerShape(15.dp))
            .border(1.dp, if (done) Theme.edgeDone(hue) else hue.copy(alpha = 0.24f), RoundedCornerShape(15.dp))
            .pressable(onTap = tap),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.CenterHorizontally),
    ) {
        Icon(icon, null, Modifier.size(17.dp), tint = hue)
        Text(title, color = hue, fontSize = 14.sp, fontWeight = FontWeight.Bold, maxLines = 1)
    }
}

/** Негромкая кнопка внутри панели — для РЕДКИХ действий («Новый код»). */
@Composable
fun QuietButton(icon: ImageVector, title: String, tap: () -> Unit) {
    var did by remember { mutableStateOf(false) }
    LaunchedEffect(did) { if (did) { delay(Theme.COPIED_MS); did = false } }
    val hue = if (did) Theme.accent else Theme.dim
    Row(
        Modifier
            .fillMaxWidth()
            .height(34.dp)
            .border(1.dp, if (did) Theme.edgeDone() else Theme.hairline, RoundedCornerShape(10.dp))
            .pressable(onTap = { tap(); did = true }),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(6.dp, Alignment.CenterHorizontally),
    ) {
        Icon(if (did) Icons.Filled.Check else icon, null, Modifier.size(13.dp), tint = hue)
        Text(if (did) "Готово" else title, color = hue, fontSize = 12.5.sp, fontWeight = FontWeight.Bold)
    }
}

/** Тихий знак состояния у заголовка раздела — вместо полосы во всю ширину. */
@Composable
fun StateChip(text: String, tint: Color = Theme.amber) {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(5.dp)) {
        Box(Modifier.size(6.dp).background(tint, CircleShape))
        Text(text, color = tint, fontSize = 11.sp, fontWeight = FontWeight.Bold)
    }
}
