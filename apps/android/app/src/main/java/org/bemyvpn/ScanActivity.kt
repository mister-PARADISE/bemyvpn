package org.bemyvpn

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.os.Bundle
import android.view.Gravity
import android.view.ViewGroup.LayoutParams.MATCH_PARENT
import android.view.ViewGroup.LayoutParams.WRAP_CONTENT
import android.widget.FrameLayout
import android.widget.TextView
import android.widget.Toast
import androidx.core.view.WindowCompat
import com.google.zxing.BarcodeFormat
import com.google.zxing.ResultPoint
import com.journeyapps.barcodescanner.BarcodeCallback
import com.journeyapps.barcodescanner.BarcodeResult
import com.journeyapps.barcodescanner.BarcodeView
import com.journeyapps.barcodescanner.DefaultDecoderFactory
import com.journeyapps.barcodescanner.camera.CenterCropStrategy

/**
 * Экран сканера QR — как iOS ScannerSheet: камера НА ВЕСЬ экран, поверх неё
 * белая скруглённая рамка-прицел 230×230 по центру, подпись снизу и «Отмена»
 * сверху. По коду возвращает результат в MainActivity (extra "scan_result").
 */
class ScanActivity : Activity() {

    private lateinit var barcodeView: BarcodeView
    private var hasCam = false
    private var done = false

    private fun dp(v: Int) = (v * resources.displayMetrics.density).toInt()

    private val callback = object : BarcodeCallback {
        override fun barcodeResult(result: BarcodeResult) {
            if (done) return
            val text = result.text
            if (text.isNullOrBlank()) { barcodeView.decodeSingle(this); return }
            done = true
            setResult(RESULT_OK, Intent().putExtra("scan_result", text))
            finish()
        }
        override fun possibleResultPoints(resultPoints: MutableList<ResultPoint>) {}
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        WindowCompat.setDecorFitsSystemWindows(window, false)

        val root = FrameLayout(this).apply {
            setBackgroundColor(Color.BLACK)
            layoutParams = FrameLayout.LayoutParams(MATCH_PARENT, MATCH_PARENT)
        }

        // Камера на весь экран.
        barcodeView = BarcodeView(this).apply {
            decoderFactory = DefaultDecoderFactory(listOf(BarcodeFormat.QR_CODE))
            setPreviewScalingStrategy(CenterCropStrategy())
            layoutParams = FrameLayout.LayoutParams(MATCH_PARENT, MATCH_PARENT)
        }
        root.addView(barcodeView)

        // Белая скруглённая рамка-прицел по центру (как на iOS).
        root.addView(FrameLayout(this).apply {
            background = GradientDrawable().apply {
                setColor(Color.TRANSPARENT)
                cornerRadius = dp(20).toFloat()
                setStroke(dp(3), 0xE6FFFFFF.toInt())
            }
            layoutParams = FrameLayout.LayoutParams(dp(230), dp(230), Gravity.CENTER)
        })

        // Подпись снизу.
        root.addView(TextView(this).apply {
            text = "Наведите камеру на QR приглашения"
            setTextColor(Color.WHITE); textSize = 14f
            setPadding(dp(12), dp(10), dp(12), dp(10))
            background = GradientDrawable().apply {
                setColor(0x80000000.toInt()); cornerRadius = dp(10).toFloat()
            }
            layoutParams = FrameLayout.LayoutParams(WRAP_CONTENT, WRAP_CONTENT, Gravity.BOTTOM or Gravity.CENTER_HORIZONTAL)
                .apply { bottomMargin = dp(60) }
        })

        // Шапка: «Отмена» слева, заголовок по центру.
        root.addView(TextView(this).apply {
            text = "Отмена"
            setTextColor(0xFF5E93FF.toInt()); textSize = 16f
            setPadding(dp(16), dp(52), dp(16), dp(12))
            layoutParams = FrameLayout.LayoutParams(WRAP_CONTENT, WRAP_CONTENT, Gravity.TOP or Gravity.START)
            setOnClickListener { finish() }
        })
        root.addView(TextView(this).apply {
            text = "Сканировать QR"
            setTextColor(Color.WHITE); textSize = 17f
            setTypeface(typeface, Typeface.BOLD)
            setPadding(0, dp(52), 0, dp(12))
            layoutParams = FrameLayout.LayoutParams(WRAP_CONTENT, WRAP_CONTENT, Gravity.TOP or Gravity.CENTER_HORIZONTAL)
        })

        setContentView(root)

        if (checkSelfPermission(Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED) {
            hasCam = true
        } else {
            requestPermissions(arrayOf(Manifest.permission.CAMERA), 1)
        }
    }

    override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, grantResults: IntArray) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (grantResults.isNotEmpty() && grantResults[0] == PackageManager.PERMISSION_GRANTED) {
            hasCam = true
            barcodeView.resume(); barcodeView.decodeSingle(callback)
        } else {
            Toast.makeText(this, "Нет доступа к камере", Toast.LENGTH_SHORT).show(); finish()
        }
    }

    override fun onResume() {
        super.onResume()
        if (hasCam && !done) { barcodeView.resume(); barcodeView.decodeSingle(callback) }
    }

    override fun onPause() {
        super.onPause()
        barcodeView.pause()
    }
}
