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
import org.bemyvpn.Theme

// ── Значки протоколов (аналоги SF Symbols из iOS) ─────────────────────────────
// Семейство ЩИТА — уровень защиты трафика: есть / замаскирована / нет.
fun protoIcon(p: String): ImageVector = when (p) {
    "noise", "noise-aes" -> Icons.Filled.Security          // защищено (щит с замком)
    "noise-obfs" -> MaskIcon                               // «Скрытный» — маска (как iOS theatermasks)
    "plain", "" -> Icons.Filled.RemoveModerator            // защиты нет (щит перечёркнут)
    else -> Icons.AutoMirrored.Filled.HelpOutline
}

/** Маскарадная маска — «Скрытный» протокол (обфускация/маскировка). Аккуратный
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
    Column(Modifier.fillMaxWidth().padding(horizontal = 11.dp, vertical = 9.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Text(label, color = Theme.dim, fontSize = 10.sp, fontWeight = FontWeight.Black, letterSpacing = 0.7.sp)
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(5.dp)) {
            if (symbol != null) Icon(symbol, null, Modifier.size(14.dp), tint = valueColor)
            // weight(1f): значение занимает всё свободное место (Spacer на iOS),
            // прижимая trailing-иконку к правому краю плитки.
            Text(
                value, color = valueColor, fontSize = 13.5.sp, fontWeight = FontWeight.SemiBold,
                fontFamily = if (mono) FontFamily.Monospace else null,
                maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f),
            )
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

/** Крупная кнопка «скопировать» — градиент, подтверждение зелёным. */
@Composable
fun BigCopyButton(value: String, modifier: Modifier = Modifier) {
    val clipboard = LocalClipboardManager.current
    val ctx = LocalContext.current
    var copied by remember { mutableStateOf(false) }
    LaunchedEffect(copied) { if (copied) { delay(1400); copied = false } }
    val bg = if (copied) Modifier.background(Theme.green, RoundedCornerShape(14.dp))
    else Modifier.background(Brush.horizontalGradient(listOf(Theme.accent, Theme.accent2)), RoundedCornerShape(14.dp))
    Row(
        modifier.then(bg).pressable {
            if (value.isNotEmpty()) { clipboard.setText(AnnotatedString(value)); Haptics.success(ctx); copied = true }
        }.padding(vertical = 15.dp),
        horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.CenterHorizontally),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(if (copied) Icons.Filled.Check else Icons.Filled.ContentCopy, null, Modifier.size(17.dp), tint = Color.White)
        Text(if (copied) "Скопировано" else "Скопировать код", color = Color.White, fontSize = 16.sp, fontWeight = FontWeight.Bold)
    }
}

/** Градиентная главная кнопка (текст по центру). */
@Composable
fun GradientButton(title: String, modifier: Modifier = Modifier, enabled: Boolean = true, onTap: () -> Unit) {
    Box(
        modifier
            .fillMaxWidth()
            .alpha(if (enabled) 1f else 0.5f)
            .background(Brush.horizontalGradient(listOf(Theme.accent, Theme.accent2)), RoundedCornerShape(14.dp))
            .pressable(enabled = enabled, onTap = onTap)
            .padding(vertical = 15.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(title, color = Color.White, fontSize = 16.sp, fontWeight = FontWeight.Bold)
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
