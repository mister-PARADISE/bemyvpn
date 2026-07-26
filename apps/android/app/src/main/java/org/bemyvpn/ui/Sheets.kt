package org.bemyvpn.ui

import android.graphics.Bitmap
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.FilterQuality
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import com.google.zxing.BarcodeFormat
import com.google.zxing.EncodeHintType
import com.google.zxing.qrcode.QRCodeWriter
import org.bemyvpn.Theme

/** QR-картинка из кода (deep-link bemyvpn://CODE — ловит встроенный сканер). */
private fun qrBitmap(code: String): Bitmap? = try {
    // 720×720 — кратно типичному размеру матрицы: модули остаются резкими
    // (плюс FilterQuality.None при отрисовке, как interpolation .none на iOS).
    val hints = mapOf(EncodeHintType.MARGIN to 0)
    val m = QRCodeWriter().encode("bemyvpn://$code", BarcodeFormat.QR_CODE, 720, 720, hints)
    val w = m.width; val h = m.height
    val px = IntArray(w * h) { i ->
        if (m.get(i % w, i / w)) 0xFF000000.toInt() else 0xFFFFFFFF.toInt()
    }
    Bitmap.createBitmap(px, w, h, Bitmap.Config.ARGB_8888)
} catch (_: Throwable) { null }

/** Показать код сети как QR — гость наводит камеру и подключается (iOS QRSheet). */
@Composable
fun QrSheet(code: String, onClose: () -> Unit) {
    Dialog(onDismissRequest = onClose, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        Column(Modifier.fillMaxSize().background(Theme.bg).statusBarsPadding()) {
            // Шапка: «Закрыть» слева, заголовок по центру (как навбар шита iOS).
            Box(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 14.dp)) {
                Text(
                    "Закрыть", color = Theme.accent, fontSize = 16.sp,
                    modifier = Modifier.align(Alignment.CenterStart).tappable(onClose),
                )
                Text(
                    "Приглашение", color = Theme.fg, fontSize = 17.sp, fontWeight = FontWeight.SemiBold,
                    modifier = Modifier.align(Alignment.Center),
                )
            }
            Column(
                Modifier.fillMaxSize().padding(16.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(22.dp),
            ) {
                Box(Modifier.weight(1f))
                val bmp = remember(code) { qrBitmap(code) }
                if (bmp != null) {
                    Box(Modifier.background(Color.White, RoundedCornerShape(18.dp)).padding(16.dp)) {
                        Image(
                            bmp.asImageBitmap(), null, Modifier.size(240.dp),
                            contentScale = ContentScale.Fit, filterQuality = FilterQuality.None,
                        )
                    }
                }
                Text(
                    code, color = Theme.accent, fontSize = 26.sp, fontWeight = FontWeight.ExtraBold,
                    fontFamily = FontFamily.Monospace, letterSpacing = 2.sp,
                )
                Text("Отсканируйте, чтобы подключиться к этой сети", color = Theme.dim, fontSize = 13.sp)
                BigCopyButton(code, Modifier.fillMaxWidth().padding(horizontal = 40.dp))
                Box(Modifier.weight(2f))
            }
        }
    }
}
