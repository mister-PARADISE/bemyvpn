package org.bemyvpn

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Intent
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.VpnService
import android.os.Build
import android.os.ParcelFileDescriptor
import java.util.concurrent.atomic.AtomicInteger

/**
 * VpnService BeMyVPN — ДВУХФАЗНЫЙ, чтобы провал подключения не ломал интернет
 * (та же схема, что iOS PacketTunnelProvider):
 *
 *   1) nativeConnect — пробить NAT + рукопожатие, БЕЗ TUN. Провал → просто гасим
 *      сервис, маршрутизация не тронута, интернет жив.
 *   2) только при успехе — establish() создаёт TUN → nativeStartTunnel качает
 *      (внутри ядра pump_tunnel с АВТО-РЕКОННЕКТОМ: смена вышки/сети не роняет TUN).
 *   3) МОНИТОР статуса ядра: туннель оборвался/остановлен (даже когда приложение
 *      свёрнуто) — закрываем всё и убираем уведомление.
 *   4) Наблюдение за сетью (аналог NWPathMonitor): смена интерфейса → nativeNudge.
 *
 * ГОНКА «отменил во время подключения» (важно): STOP мог прийти, пока поток
 * подключения между фазами. Раньше поток продолжал и СОЗДАВАЛ TUN уже ПОСЛЕ
 * stopVpn → в системе висел активный VPN, а интернет пропадал. Теперь у каждой
 * попытки своё ПОКОЛЕНИЕ (gen); stopVpn его инкрементит; поток на каждом рубеже
 * под `lock` проверяет своё поколение и, если устарел, закрывает СВОЙ TUN-fd и
 * выходит, ничего не поднимая. Стоп — авторитетный, fd всегда закрывается.
 */
class BmvVpnService : VpnService() {

    private val gen = AtomicInteger(0)
    private val lock = Any()
    @Volatile private var monitor: Thread? = null
    @Volatile private var tunFd: ParcelFileDescriptor? = null
    private var netCallback: ConnectivityManager.NetworkCallback? = null
    @Volatile private var lastNetKey = ""

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            stopVpn()
            return START_NOT_STICKY
        }
        val host = intent?.getStringExtra(EXTRA_HOST)
        val coordinator = intent?.getStringExtra(EXTRA_COORDINATOR) ?: Config.defaultCoordinator(this)
        val password = intent?.getStringExtra(EXTRA_PASSWORD) ?: ""
        val protocol = intent?.getStringExtra(EXTRA_PROTOCOL) ?: ""
        // Новая попытка → своё поколение (инвалидирует прежние in-flight потоки).
        val myGen = gen.incrementAndGet()
        // foreground СРАЗУ (обязательство перед системой — успеть за ~5с).
        foreground("Подключаюсь…")
        if (host.isNullOrBlank()) {
            stopVpn()
            return START_NOT_STICKY
        }
        startConnecting(myGen, coordinator, host, password, protocol)
        return START_STICKY
    }

    private fun stale(myGen: Int) = myGen != gen.get()

    private fun startConnecting(myGen: Int, coordinator: String, host: String, password: String, protocol: String) {
        Thread {
            // ФАЗА 1: соединиться БЕЗ TUN.
            val ok = try { Native.nativeConnect(coordinator, host, password, protocol) } catch (_: Throwable) { false }
            // Отменили за время рукопожатия → ничего не поднимаем (stopVpn уже прошёл).
            if (stale(myGen)) return@Thread
            if (!ok) { stopVpn(); return@Thread }

            // ФАЗА 2: канал есть → создаём TUN.
            val pfd = try {
                Builder()
                    .setSession("BeMyVPN")
                    .setMtu(1400)
                    .addAddress("10.7.0.2", 24)
                    .addRoute("0.0.0.0", 0)
                    .addDnsServer("8.8.8.8")
                    .addDisallowedApplication(packageName)
                    .establish()
            } catch (_: Throwable) { null }
            if (pfd == null) { if (!stale(myGen)) stopVpn(); return@Thread }

            // Публикуем TUN и запускаем перекачку — под lock, атомарно с проверкой
            // поколения: если стоп случился прямо сейчас, закрываем СВОЙ fd и выходим.
            val fd: Int
            synchronized(lock) {
                if (stale(myGen)) { try { pfd.close() } catch (_: Throwable) {}; return@Thread }
                tunFd = pfd
                fd = pfd.detachFd()
            }
            foreground("Защищено")
            val started = try { Native.nativeStartTunnel(fd) } catch (_: Throwable) { false }
            // Стоп во время старта туннеля → tunFd уже у native (fd), гасим по-настоящему.
            if (stale(myGen) || !started) { stopVpn(); return@Thread }
            startMonitor(myGen)
            startNetWatch()
        }.also { it.isDaemon = true }.start()
    }

    /** Монитор: гасит всё, когда ядро сообщает 0 (стоп) или 3 (ошибка). */
    private fun startMonitor(myGen: Int) {
        val m = Thread {
            while (!Thread.currentThread().isInterrupted) {
                try { Thread.sleep(700) } catch (_: InterruptedException) { break }
                if (stale(myGen)) break // нас сменила новая попытка/стоп
                val s = try { Native.nativeVpnStatus() } catch (_: Throwable) { 0 }
                if (s == 0 || s == 3) { stopVpn(); break }
            }
        }
        m.isDaemon = true
        monitor = m
        m.start()
    }

    /** Смена интерфейса (WiFi↔сотовая) → nativeNudge (реконнект без падения TUN). */
    private fun startNetWatch() {
        if (netCallback != null) return
        val cm = getSystemService(ConnectivityManager::class.java) ?: return
        val cb = object : ConnectivityManager.NetworkCallback() {
            override fun onCapabilitiesChanged(network: Network, caps: NetworkCapabilities) {
                val key = buildString {
                    if (caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI)) append('w')
                    if (caps.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR)) append('c')
                    if (caps.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET)) append('e')
                }
                if (lastNetKey.isEmpty()) { lastNetKey = key; return }
                if (key != lastNetKey) {
                    lastNetKey = key
                    try { Native.nativeNudge() } catch (_: Throwable) {}
                }
            }
        }
        try {
            cm.registerDefaultNetworkCallback(cb)
            netCallback = cb
        } catch (_: Throwable) {}
    }

    private fun stopNetWatch() {
        netCallback?.let {
            try { getSystemService(ConnectivityManager::class.java)?.unregisterNetworkCallback(it) } catch (_: Throwable) {}
        }
        netCallback = null
        lastNetKey = ""
    }

    private fun stopVpn() {
        gen.incrementAndGet() // инвалидируем ЛЮБЫЕ идущие попытки подключения
        monitor?.interrupt(); monitor = null
        stopNetWatch()
        // Синхронно прощаемся с хостом (BYE) и гасим сессию ядра.
        try { Native.nativeStop() } catch (_: Throwable) {}
        // Закрыть TUN-fd (если наш) — иначе система держит VPN активным, а трафик
        // уходит в мёртвый туннель = «нет интернета».
        synchronized(lock) {
            try { tunFd?.close() } catch (_: Throwable) {}
            tunFd = null
        }
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    override fun onDestroy() {
        stopVpn()
        super.onDestroy()
    }

    // Пользователь выключил VPN из системных настроек — BYE уйдёт и отсюда.
    override fun onRevoke() {
        stopVpn()
        super.onRevoke()
    }

    private fun foreground(text: String) {
        val chanId = "bmv"
        if (Build.VERSION.SDK_INT >= 26) {
            val chan = NotificationChannel(chanId, "BeMyVPN", NotificationManager.IMPORTANCE_LOW)
            getSystemService(NotificationManager::class.java).createNotificationChannel(chan)
        }
        val builder = if (Build.VERSION.SDK_INT >= 26) {
            Notification.Builder(this, chanId)
        } else {
            @Suppress("DEPRECATION")
            Notification.Builder(this)
        }
        val notif = builder
            .setContentTitle("BeMyVPN")
            .setContentText(text)
            .setSmallIcon(android.R.drawable.ic_lock_idle_lock)
            .setOngoing(true)
            .build()
        startForeground(1, notif)
    }

    companion object {
        const val EXTRA_HOST = "host"
        const val EXTRA_COORDINATOR = "coordinator"
        const val EXTRA_PASSWORD = "password"
        const val EXTRA_PROTOCOL = "protocol"
        const val ACTION_STOP = "org.bemyvpn.STOP"
    }
}
