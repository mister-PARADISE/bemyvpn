package org.bemyvpn

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import android.os.PowerManager

/**
 * Общее состояние хост-режима (сервис пишет, AppState читает).
 * result — вердикт запуска: «код|подпись», "!NAT", "!SIG" или "" (ошибка).
 */
object HostState {
    @Volatile var running = false
    @Volatile var result: String? = null
}

/**
 * Foreground-сервис хост-режима: телефон работает чьим-то VPN. VPN-права НЕ нужны —
 * хост работает через обычные сокеты (userspace-стек в ядре). Foreground +
 * START_STICKY + параметры из prefs → раздача живёт, когда приложение свёрнуто,
 * и поднимается заново, если сервис перезапустила система.
 *
 * Код/подпись выдаёт СЕРВЕР (AppState.ensureHostCode). Если подпись протухла
 * (сменился секрет координатора → анонс 403 → "!SIG"), сервис сам берёт свежий
 * код и повторяет один раз — хост не залипает навсегда.
 */
class HostService : Service() {

    private var wakeLock: PowerManager.WakeLock? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            stopHost()
            return START_NOT_STICKY
        }
        val prefs = getSharedPreferences("bmv", MODE_PRIVATE)
        // Система перезапустила нас с null-intent → берём параметры из prefs.
        val coordinator = intent?.getStringExtra(EXTRA_COORDINATOR)
            ?: prefs.getString("coordinator", null) ?: Config.defaultCoordinator(this)
        val id0 = intent?.getStringExtra(EXTRA_ID) ?: prefs.getString("host_code", "") ?: ""
        val sig0 = intent?.getStringExtra(EXTRA_SIG) ?: prefs.getString("host_sig", "") ?: ""
        val token = intent?.getStringExtra(EXTRA_TOKEN) ?: prefs.getString("host_token", "") ?: ""
        // Имя уходит в ПУБЛИЧНЫЙ каталог. Раньше при перезапуске системой сюда
        // подставлялась модель телефона — человек её не выбирал и не увидел бы,
        // пока не открыл список. Источник один: те же prefs и то же умолчание,
        // что у AppState.
        val name = intent?.getStringExtra(EXTRA_NAME) ?: prefs.getString("host_name", null) ?: DEFAULT_NAME
        val max = intent?.getIntExtra(EXTRA_MAX, 8) ?: prefs.getInt("host_max", 8)
        val password = intent?.getStringExtra(EXTRA_PASSWORD) ?: prefs.getString("host_pw", "") ?: ""
        val protocol = intent?.getStringExtra(EXTRA_PROTOCOL)
            ?: prefs.getString("host_proto", null) ?: DEFAULT_PROTOCOL
        val public = intent?.getBooleanExtra(EXTRA_PUBLIC, true) ?: prefs.getBoolean("host_public", true)

        foreground()
        acquireWakeLock() // чтобы heartbeat не «засыпал» в Doze при погашенном экране

        Thread {
            fun freshCode(): Pair<String, String> {
                val parts = (try { Native.nativeNewCode(coordinator) } catch (_: Throwable) { "" }).split("|")
                val id = parts.getOrElse(0) { "" }; val sig = parts.getOrElse(1) { "" }
                if (id.isNotBlank() && sig.isNotBlank()) {
                    prefs.edit().putString("host_code", id).putString("host_sig", sig).apply()
                }
                return id to sig
            }
            fun tryStart(id: String, sig: String): String = try {
                Native.nativeHostStart(coordinator, id, token, sig, name, max, password, protocol, public)
            } catch (_: Throwable) { "" }

            var stableId = id0
            var codeSig = sig0
            if (stableId.isBlank() || codeSig.isBlank()) { val f = freshCode(); stableId = f.first; codeSig = f.second }

            var id = if (stableId.isBlank()) "" else tryStart(stableId, codeSig)
            if (id == "!SIG") {
                // Подпись протухла — самоисцеление: свежий код, одна повторная попытка.
                val f = freshCode()
                if (f.first.isNotBlank() && f.second.isNotBlank()) {
                    stableId = f.first; codeSig = f.second
                    id = tryStart(stableId, codeSig)
                }
            }
            if (id.isEmpty() || id.startsWith("!")) {
                HostState.running = false
                HostState.result = id
                releaseWakeLock()
                stopForeground(STOP_FOREGROUND_REMOVE)
                stopSelf()
            } else {
                // Ядро отдаёт «код|подпись» целиком — разбирает и сохраняет AppState.
                HostState.running = true
                HostState.result = id
                // Время старта раздачи — для «РАЗДАЮ …» на вкладке Хост.
                prefs.edit().putLong("host_started_at", System.currentTimeMillis()).apply()
            }
        }.start()
        return START_STICKY
    }

    private fun stopHost() {
        try { Native.nativeHostStop() } catch (_: Throwable) {}
        HostState.running = false
        getSharedPreferences("bmv", MODE_PRIVATE).edit().remove("host_started_at").apply()
        releaseWakeLock()
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    private fun acquireWakeLock() {
        if (wakeLock?.isHeld == true) return
        try {
            val pm = getSystemService(POWER_SERVICE) as PowerManager
            wakeLock = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "bemyvpn:host").apply {
                setReferenceCounted(false); acquire()
            }
        } catch (_: Throwable) {}
    }

    private fun releaseWakeLock() {
        try { if (wakeLock?.isHeld == true) wakeLock?.release() } catch (_: Throwable) {}
        wakeLock = null
    }

    private fun foreground() {
        val chanId = "bmv-host"
        if (Build.VERSION.SDK_INT >= 26) {
            val chan = NotificationChannel(chanId, "BeMyVPN Хост", NotificationManager.IMPORTANCE_LOW)
            getSystemService(NotificationManager::class.java).createNotificationChannel(chan)
        }
        val builder = if (Build.VERSION.SDK_INT >= 26) {
            Notification.Builder(this, chanId)
        } else {
            @Suppress("DEPRECATION")
            Notification.Builder(this)
        }
        val notif = builder
            .setContentTitle("BeMyVPN — хост")
            // Не «Вы раздаёте интернет»: ничего вы не раздаёте — через ваш
            // канал ходит в сеть тот, кому свой перекрыли.
            .setContentText("Вы работаете чьим-то VPN")
            .setSmallIcon(android.R.drawable.ic_menu_share)
            .setOngoing(true)
            .build()
        if (Build.VERSION.SDK_INT >= 34) {
            startForeground(2, notif, ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE)
        } else {
            startForeground(2, notif)
        }
    }

    companion object {
        const val EXTRA_COORDINATOR = "coordinator"
        const val EXTRA_ID = "id"
        const val EXTRA_TOKEN = "token"
        const val EXTRA_SIG = "sig"
        const val EXTRA_NAME = "name"
        const val EXTRA_MAX = "max"
        const val EXTRA_PASSWORD = "password"
        const val EXTRA_PROTOCOL = "protocol"
        const val EXTRA_PUBLIC = "public"
        const val ACTION_STOP = "org.bemyvpn.HOST_STOP"
        /** Умолчание имени хоста — одно на приложение (см. AppState.hostName). */
        const val DEFAULT_NAME = "Хост"

        /**
         * Умолчание протокола раздачи — ОДНО на приложение (см. AppState.hostProtocol).
         *
         * Здесь и в AppState умолчания были РАЗНЫЕ под одним ключом настроек
         * («noise» против «noise-obfs»), и раздача, поднятая системой заново,
         * уезжала на другой протокол, чем показывал экран. Значение живёт в ядре
         * (bmv_config::DEFAULT_PROTOCOL), но двери в мост у него пока нет —
         * поэтому строка одна и здесь.
         */
        const val DEFAULT_PROTOCOL = "noise-obfs"
    }
}
