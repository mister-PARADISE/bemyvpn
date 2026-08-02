package org.bemyvpn.ui

import androidx.compose.animation.animateContentSize
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
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material.icons.filled.RemoveModerator
import androidx.compose.material.icons.filled.Security
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.composed
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
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
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.material.icons.filled.QrCode2
import org.bemyvpn.Theme

// ── Значки протоколов (аналоги SF Symbols из iOS) ─────────────────────────────
// Семейство ЩИТА — уровень защиты трафика: есть / замаскирована / нет.
fun protoIcon(p: String): ImageVector = when (p) {
    "noise", "noise-aes" -> Icons.Filled.Security          // защищено (щит с замком)
    "noise-obfs" -> MaskIcon                               // «Маскировка» — маска (как iOS theatermasks)
    "plain", "" -> Icons.Filled.RemoveModerator            // защиты нет (щит перечёркнут)
    else -> Icons.AutoMirrored.Filled.HelpOutline
}

/** Маскарадная маска — «Маскировка» протокол (обфускация/маскировка). Аккуратный
 *  вектор с прорезями-глазами (evenodd), чистый на любом размере. Аналог iOS
 *  theatermasks.fill, но без «мусора» на мелких размерах. */
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

// ── Точка статуса (с пульсом) ────────────────────────────────────────────────
@Composable
fun Dot(color: Color, pulse: Boolean = false) {
    val alpha = if (pulse) {
        val inf = rememberInfiniteTransition(label = "dot")
        val a by inf.animateFloat(1f, 0.35f, infiniteRepeatable(tween(1000), RepeatMode.Reverse), label = "dotA")
        a
    } else 1f
    Box(Modifier.size(11.dp).alpha(alpha).background(color, CircleShape))
}

// ── Секундный тикер (аналог TimelineView .periodic 1с) ───────────────────────
@Composable
fun rememberSecondTick(): Long {
    var tick by remember { mutableLongStateOf(System.currentTimeMillis()) }
    LaunchedEffect(Unit) { while (true) { delay(1000); tick = System.currentTimeMillis() } }
    return tick
}

// ── Плитки ───────────────────────────────────────────────────────────────────

/** Фон плитки: единый для всех — разница только в содержимом, не в оформлении. */
fun Modifier.tileBackground(accent: Color? = null): Modifier = this
    .background(Theme.tile, RoundedCornerShape(12.dp))
    .border(1.dp, accent ?: Color.White.copy(alpha = 0.07f), RoundedCornerShape(12.dp))

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
                )
            }
            trailing()
        }
    }
}

/** Плитка-факт. */
@Composable
fun StatTile(label: String, value: String, modifier: Modifier = Modifier, symbol: ImageVector? = null, tint: Color = Theme.fg) {
    Box(modifier.tileBackground()) { TileBody(label, value, symbol, tint) }
}

/** Плитка-кнопка: тап запускает действие (например, перепроверить пинг). */
@Composable
fun ActionTile(
    label: String, value: String, modifier: Modifier = Modifier,
    tint: Color = Theme.fg, icon: ImageVector, busy: Boolean = false, action: () -> Unit,
) {
    val ctx = LocalContext.current
    Box(modifier.tileBackground().pressable { Haptics.tap(ctx); action() }) {
        TileBody(label, if (busy) "проверяю…" else value, valueColor = if (busy) Theme.dim else tint) {
            if (busy) CircularProgressIndicator(Modifier.size(12.dp), color = Theme.accent, strokeWidth = 1.5.dp)
            else Icon(icon, null, Modifier.size(13.dp), tint = Theme.accent)
        }
    }
}

/** Плитка со значением, которое копируется тапом (код, IP). */
@Composable
fun CopyTile(label: String, value: String, modifier: Modifier = Modifier) {
    val clipboard = LocalClipboardManager.current
    val ctx = LocalContext.current
    var copied by remember { mutableStateOf(false) }
    val empty = value.isEmpty() || value == "—"
    LaunchedEffect(copied) { if (copied) { delay(1300); copied = false } }
    Box(modifier.tileBackground(if (copied) Theme.green.copy(alpha = 0.5f) else null).tappable {
        if (!empty) { clipboard.setText(AnnotatedString(value)); Haptics.tap(ctx); copied = true }
    }) {
        TileBody(
            label, if (copied) "Скопировано" else value,
            valueColor = if (copied) Theme.green else Theme.fg, mono = !copied,
        ) {
            if (!empty) Icon(
                if (copied) Icons.Filled.Check else Icons.Filled.ContentCopy, null,
                Modifier.size(13.dp), tint = if (copied) Theme.green else Theme.accent,
            )
        }
    }
}

// ── Кнопки ───────────────────────────────────────────────────────────────────

/**
 * Кнопка в карточке: значок + подпись, приподнятая поверхность, отклик на палец.
 * Если задан copy, кладёт его в буфер и на 1.3с превращается в «Скопировано ✓».
 */
@Composable
fun CardButton(icon: ImageVector, title: String, modifier: Modifier = Modifier, copy: String? = null, action: (() -> Unit)? = null) {
    val clipboard = LocalClipboardManager.current
    val ctx = LocalContext.current
    var copied by remember { mutableStateOf(false) }
    LaunchedEffect(copied) { if (copied) { delay(1300); copied = false } }
    Row(
        modifier
            .background(Theme.tile, RoundedCornerShape(12.dp))
            .border(1.dp, if (copied) Theme.green.copy(alpha = 0.5f) else Color.White.copy(alpha = 0.08f), RoundedCornerShape(12.dp))
            .pressable {
                if (copy != null) {
                    if (copy.isNotEmpty()) { clipboard.setText(AnnotatedString(copy)); Haptics.success(ctx); copied = true }
                } else { Haptics.tap(ctx); action?.invoke() }
            }
            .padding(vertical = 12.dp),
        horizontalArrangement = Arrangement.spacedBy(7.dp, Alignment.CenterHorizontally),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(if (copied) Icons.Filled.Check else icon, null, Modifier.size(15.dp), tint = if (copied) Theme.green else Theme.accent)
        Text(
            if (copied) "Скопировано" else title, color = if (copied) Theme.green else Theme.fg,
            fontSize = 14.sp, fontWeight = FontWeight.Bold, maxLines = 1, overflow = TextOverflow.Ellipsis,
        )
    }
}

/** Крупная кнопка «скопировать» под QR-кодом. Спокойная, как и все остальные:
 *  подкраска + рамка + цветной текст. Сплошной градиент был единственным ярким
 *  пятном на почти пустом экране и перетягивал взгляд с самого кода. */
@Composable
fun BigCopyButton(value: String, modifier: Modifier = Modifier) {
    val clipboard = LocalClipboardManager.current
    val ctx = LocalContext.current
    var copied by remember { mutableStateOf(false) }
    LaunchedEffect(copied) { if (copied) { delay(1400); copied = false } }
    val hue = if (copied) Theme.green else Theme.accent
    Row(
        modifier
            .background(hue.copy(alpha = 0.15f), RoundedCornerShape(14.dp))
            .border(1.dp, hue.copy(alpha = 0.4f), RoundedCornerShape(14.dp))
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
            .background(Theme.accent.copy(alpha = 0.15f), RoundedCornerShape(14.dp))
            .border(1.dp, Theme.accent.copy(alpha = 0.4f), RoundedCornerShape(14.dp))
            .pressable(enabled = enabled, onTap = onTap)
            .padding(vertical = 15.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(title, color = Theme.accent, fontSize = 16.sp, fontWeight = FontWeight.Bold)
    }
}

// ── Карточка и подписи ───────────────────────────────────────────────────────

@Composable
fun Card(modifier: Modifier = Modifier, content: @Composable () -> Unit) {
    Column(
        modifier.fillMaxWidth().background(Theme.panel, RoundedCornerShape(16.dp)).padding(16.dp).animateContentSize(),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) { content() }
}

@Composable
fun SectionLabel(t: String) {
    Text(
        t, color = Theme.dim, fontSize = 13.sp, fontWeight = FontWeight.Bold,
        modifier = Modifier.fillMaxWidth().padding(top = 6.dp),
    )
}

@Composable
fun TabHeader(icon: ImageVector, title: String) {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
        Icon(icon, null, Modifier.size(26.dp), tint = Theme.accent)
        Text(title, color = Theme.fg, fontSize = 26.sp, fontWeight = FontWeight.ExtraBold)
    }
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
 * Обёртка прижатой панели: она лежит ВНЕ прокрутки, поэтому статус не уезжает
 * при листании, а содержимое уходит под неё.
 */
@Composable
fun PinnedPanel(tint: Color, content: @Composable ColumnScope.() -> Unit) {
    Box(
        Modifier
            .fillMaxWidth()
            .background(Theme.bg)
            .padding(start = 20.dp, end = 20.dp, top = 20.dp, bottom = 12.dp),
    ) {
        Column(
            Modifier
                .fillMaxWidth()
                .background(
                    Brush.verticalGradient(listOf(Theme.panel, Theme.card)),
                    RoundedCornerShape(22.dp),
                )
                .border(1.dp, tint.copy(alpha = 0.2f), RoundedCornerShape(22.dp))
                .padding(vertical = 16.dp, horizontal = 18.dp)
                .animateContentSize(),
            verticalArrangement = Arrangement.spacedBy(8.dp),
            content = content,
        )
    }
}

/**
 * Строка состояния для РАБОТАЮЩЕГО режима: значок кружком, название, часы.
 *
 * Значок никуда не девается и в работе — он опознаёт экран с одного взгляда. Но
 * держать 84dp картинки там, где нужны код и цифры, расточительно: панель прижата
 * к верху и висит на экране постоянно.
 */
@Composable
fun StatusLine(icon: ImageVector, title: String, tint: Color, clock: String? = null) {
    Row(
        Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Box(
            Modifier
                .size(38.dp)
                .background(tint.copy(alpha = 0.13f), CircleShape)
                .border(1.dp, tint.copy(alpha = 0.3f), CircleShape),
            contentAlignment = Alignment.Center,
        ) { Icon(icon, null, Modifier.size(19.dp), tint = tint) }
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
    LaunchedEffect(copied) { if (copied) { delay(1300); copied = false } }
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
private fun ShareButton(icon: ImageVector, title: String, green: Boolean, modifier: Modifier, tap: () -> Unit) {
    val hue = if (green) Theme.green else Theme.accent
    Row(
        modifier
            .height(48.dp)
            .background(Color(0xFF1B2333), RoundedCornerShape(15.dp))
            .border(1.dp, hue.copy(alpha = if (green) 0.5f else 0.24f), RoundedCornerShape(15.dp))
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
    LaunchedEffect(did) { if (did) { delay(1300); did = false } }
    val hue = if (did) Theme.green else Theme.dim
    Row(
        Modifier
            .fillMaxWidth()
            .height(34.dp)
            .border(1.dp, if (did) Theme.green.copy(alpha = 0.5f) else Color.White.copy(alpha = 0.08f), RoundedCornerShape(10.dp))
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
