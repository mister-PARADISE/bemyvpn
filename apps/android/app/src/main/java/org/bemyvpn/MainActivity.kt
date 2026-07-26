package org.bemyvpn

import android.content.Intent
import android.net.Uri
import android.net.VpnService
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsControllerCompat
import org.bemyvpn.ui.App

/**
 * Единственный экран приложения — Compose-корень (ui/App.kt), построчная копия
 * iOS ContentView. Activity отвечает только за системные интеграции: VPN-согласие,
 * deep-link bemyvpn://, сканер QR, разрешение на уведомления.
 */
class MainActivity : ComponentActivity() {

    private lateinit var app: AppState
    private var pendingHost: Host? = null
    private var pendingPassword = ""

    /** Системное согласие на VPN-профиль (первый запуск). */
    private val vpnConsent = registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { res ->
        val h = pendingHost; pendingHost = null
        if (res.resultCode == RESULT_OK && h != null) startVpnService(h, pendingPassword)
        else app.connectFailed()
    }

    /** Результат сканера QR (ScanActivity → extra "scan_result"). */
    private val scan = registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { res ->
        val text = res.data?.getStringExtra("scan_result")
        if (!text.isNullOrBlank()) handleScanned(text)
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // Edge-to-edge: тёмный фон уходит под системные панели, иконки светлые.
        WindowCompat.setDecorFitsSystemWindows(window, false)
        @Suppress("DEPRECATION")
        run { window.statusBarColor = 0; window.navigationBarColor = 0 }
        WindowInsetsControllerCompat(window, window.decorView).apply {
            isAppearanceLightStatusBars = false
            isAppearanceLightNavigationBars = false
        }

        app = AppState.get(this)
        app.onStartVpn = { host, pw -> requestVpn(host, pw) }
        app.start()

        // Уведомление foreground-сервиса (Android 13+): просим один раз на старте.
        if (Build.VERSION.SDK_INT >= 33 &&
            checkSelfPermission(android.Manifest.permission.POST_NOTIFICATIONS) != android.content.pm.PackageManager.PERMISSION_GRANTED
        ) {
            requestPermissions(arrayOf(android.Manifest.permission.POST_NOTIFICATIONS), 1)
        }

        handleIntent(intent)
        setContent {
            App(app) { scan.launch(Intent(this, ScanActivity::class.java)) }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handleIntent(intent)
    }

    /** Deep-link bemyvpn://CODE и bemyvpn://connect?code=CODE. */
    private fun handleIntent(i: Intent?) {
        val data = i?.data ?: return
        if (data.scheme == "bemyvpn") app.openDeepLink(data)
    }

    private fun handleScanned(s: String) {
        val uri = try { Uri.parse(s) } catch (_: Throwable) { null }
        if (uri?.scheme == "bemyvpn") app.openDeepLink(uri) else app.connectByCode(s)
    }

    private fun requestVpn(host: Host, password: String) {
        val consent = try { VpnService.prepare(this) } catch (_: Throwable) { null }
        if (consent != null) {
            pendingHost = host; pendingPassword = password
            vpnConsent.launch(consent)
        } else {
            startVpnService(host, password)
        }
    }

    private fun startVpnService(host: Host, password: String) {
        startService(
            Intent(this, BmvVpnService::class.java)
                .putExtra(BmvVpnService.EXTRA_HOST, host.id)
                .putExtra(BmvVpnService.EXTRA_COORDINATOR, app.coordinator)
                .putExtra(BmvVpnService.EXTRA_PASSWORD, password)
                .putExtra(BmvVpnService.EXTRA_PROTOCOL, host.proto),
        )
    }
}
