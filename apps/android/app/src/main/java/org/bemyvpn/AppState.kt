package org.bemyvpn

import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.VibrationEffect
import android.os.Vibrator
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.util.Locale
import java.util.UUID

enum class Tab { SERVER, VPN, HOST }

/** Тактильная отдача на ключевые моменты — маленький штрих качества (как iOS Haptics). */
object Haptics {
    private fun vib(ctx: Context): Vibrator? = try {
        if (Build.VERSION.SDK_INT >= 31) {
            (ctx.getSystemService(Context.VIBRATOR_MANAGER_SERVICE) as? android.os.VibratorManager)?.defaultVibrator
        } else {
            @Suppress("DEPRECATION") ctx.getSystemService(Context.VIBRATOR_SERVICE) as? Vibrator
        }
    } catch (_: Throwable) { null }

    fun success(ctx: Context) {
        try {
            if (Build.VERSION.SDK_INT >= 29) vib(ctx)?.vibrate(VibrationEffect.createPredefined(VibrationEffect.EFFECT_DOUBLE_CLICK))
            else @Suppress("DEPRECATION") vib(ctx)?.vibrate(30)
        } catch (_: Throwable) {}
    }

    fun tap(ctx: Context) {
        try {
            if (Build.VERSION.SDK_INT >= 29) vib(ctx)?.vibrate(VibrationEffect.createPredefined(VibrationEffect.EFFECT_CLICK))
            else @Suppress("DEPRECATION") vib(ctx)?.vibrate(15)
        } catch (_: Throwable) {}
    }
}

/**
 * Состояние приложения — построчный перенос iOS AppState (BeMyVPNApp.swift).
 * Живёт весь процесс (переживает пересоздание Activity), сервисы пишут статусы,
 * Compose читает mutableStateOf-поля напрямую.
 */
class AppState private constructor(val ctx: Context) {

    companion object {
        @Volatile private var shared: AppState? = null
        fun get(ctx: Context): AppState =
            shared ?: synchronized(this) { shared ?: AppState(ctx.applicationContext).also { shared = it } }
    }

    private val prefs = ctx.getSharedPreferences("bmv", Context.MODE_PRIVATE)
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)

    val defaultCoordinator: String get() = Config.defaultCoordinator(ctx)

    // навигация
    var tab by mutableStateOf(Tab.VPN)

    // сервер / каталог
    var coordinator by mutableStateOf(prefs.getString("coordinator", null) ?: Config.defaultCoordinator(ctx))
    var hosts by mutableStateOf<List<Host>>(emptyList())
    var serverOnline by mutableStateOf<Boolean?>(null)
    var myIp by mutableStateOf("")
    var ping by mutableStateOf(0)
    var checking by mutableStateOf(false)

    // ── обновление приложения ────────────────────────────────────────────────
    /** Версия свежего релиза («1.6») или null — плашки нет. */
    var updateVersion by mutableStateOf<String?>(null)
    /** Тег для скачивания («v1.6»). */
    var updateTag by mutableStateOf<String?>(null)
    /** 0 простаивает · 1 качаю · 2 отдал установщику · 3 ошибка */
    var updateState by mutableStateOf(0)
    var updateError by mutableStateOf("")
    /** Скрыто крестиком — до следующего запуска не показываем. */

    /**
     * Узнать у GitHub, есть ли релиз новее. Один раз при запуске.
     * Локальные сборки («0.0») не трогаем — разработчику подсовывать релиз вредно.
     */
    fun checkUpdate() {
        if (BuildConfig.VERSION_NAME == "0.0") return
        scope.launch(Dispatchers.IO) {
            val tag = Updater.latestTag() ?: return@launch
            val ver = tag.trimStart('v')
            if (!Updater.isNewer(ver, BuildConfig.VERSION_NAME)) return@launch
            withContext(Dispatchers.Main) {
                updateVersion = ver
                updateTag = tag
            }
        }
    }

    /** Скачать и отдать системному установщику. Диалог покажет сама система. */
    fun doUpdate() {
        val tag = updateTag ?: return
        if (updateState == 1) return // уже качаем — второй раз не начинаем
        updateState = 1
        scope.launch(Dispatchers.IO) {
            try {
                val apk = Updater.download(ctx, tag)
                withContext(Dispatchers.Main) {
                    updateState = 2
                    Updater.install(ctx, apk)
                }
            } catch (e: Throwable) {
                withContext(Dispatchers.Main) {
                    updateState = 3
                    // Самая частая причина у наших пользователей — блокировка
                    // GitHub, и её же решает само приложение.
                    updateError = "Не удалось скачать — подключитесь к VPN и повторите"
                }
            }
        }
    }

    // гость / VPN
    var vpnState by mutableStateOf(0)             // 0 выкл · 1 подключаюсь · 2 канал поднят · 3 ошибка
    var connectedTo by mutableStateOf<String?>(null)
    var connectedSince by mutableStateOf<Long?>(null)
    var resolvedHost by mutableStateOf<Host?>(null)
    /** Разовое сообщение под карточкой VPN (null — сообщения нет). */
    var vpnError by mutableStateOf<String?>(null)
    /**
     * Сообщение СПОКОЙНОЕ, а не отказ — показывать приглушённым, не красным.
     *
     * Конец сеанса бывает двух совершенно разных сортов: «не смогли подключиться»
     * (отказ) и «хост сам выключил раздачу» (всё сработало правильно). Красным по
     * второму человек читает поломку там, где её нет, и идёт чинить исправное.
     */
    var vpnNoticeCalm by mutableStateOf(false)
    private var vpnErrorJob: Job? = null

    /**
     * Показать разовое сообщение об отказе — и убрать его само через несколько секунд.
     *
     * ЧЕРЕЗ vpnState=3 это делать НЕЛЬЗЯ: фоновый опрос статуса ядра каждую секунду
     * приводит vpnState к тому, что говорит ядро. А ядро тут в состоянии 0 (мы же не
     * подключались), поэтому «ошибка» либо мигала меньше секунды и человек её не
     * замечал, либо, наоборот, залипала навсегда. Сообщение живёт отдельно от
     * состояния — со своим таймером и без драки с опросом.
     */
    private fun showVpnError(text: String, calm: Boolean = false) {
        vpnError = text
        vpnNoticeCalm = calm
        vpnErrorJob?.cancel()
        vpnErrorJob = scope.launch { delay(5000); vpnError = null }
    }
    var expandedId by mutableStateOf<String?>(null)
    /** Отклик до раскрытого хоста: «24 мс» / «—» (не ответил) / «…» (первый замер).
     *  Держим ТОЛЬКО для раскрытой карточки — закрыли, значит больше не интересно. */
    var pings by mutableStateOf<Map<String, String>>(emptyMap())
    private var pingJob: Job? = null

    /**
     * Мерить отклик до хоста ПОКА ОТКРЫТА его карточка — раз в секунду.
     *
     * Не один замер с кэшем: одиночная цифра может оказаться случайной (потеря
     * пакета, всплеск очереди), а живая показывает ещё и стабильность канала —
     * для выбора это важнее среднего. Нагрузка ничтожная: восемь байт раз в
     * секунду и только для ОДНОЙ карточки; keepalive внутри рабочей сессии
     * ходит чаще.
     *
     * null останавливает замеры (карточку закрыли).
     */
    fun watchPing(h: Host?) {
        pingJob?.cancel()
        if (h == null) return
        if (h.endpoints.isEmpty()) { pings = pings + (h.id to "—"); return }
        if (pings[h.id] == null) pings = pings + (h.id to "…")
        val coord = coordinator
        pingJob = scope.launch {
            while (isActive) {
                val ms = withContext(Dispatchers.IO) {
                    try { Native.nativeProbeRtt(coord, h.id, h.endpoints) } catch (_: Throwable) { -1 }
                }
                if (!isActive) return@launch
                // Честное «не ответил»: хост может быть за таким NAT, что без
                // пробивания до него не достучаться. Выдумывать число нельзя.
                pings = pings + (h.id to if (ms >= 0) "$ms мс" else "—")
                // Пауза ПОСЛЕ замера, а не параллельно: у пробы свой срок (1.5с),
                // и запуск нового замера поверх незакрытого копил бы их.
                delay(1000)
            }
        }
    }

    // хост
    var hosting by mutableStateOf(false)
    var starting by mutableStateOf(false)
    var hostCode by mutableStateOf(prefs.getString("host_code", null) ?: "")
    var hostSig by mutableStateOf(prefs.getString("host_sig", null) ?: "")
    // Имя хоста уходит в ПУБЛИЧНЫЙ каталог, который видят все. Модель устройства
    // («Pixel 7») хотя бы не выдаёт владельца, но и не говорит ничего полезного —
    // а на других платформах сюда попадало имя машины вида «MacBook Air — Armen»,
    // то есть настоящее имя. Единое правило: страна по своему IP (офлайн, из
    // встроенной базы), до её появления — нейтральное имя.
    var hostName by mutableStateOf(prefs.getString("host_name", null) ?: "Хост")
    var hostMax by mutableStateOf(prefs.getInt("host_max", 8))
    var hostPassword by mutableStateOf(prefs.getString("host_pw", null) ?: "")
    // «noise-obfs» (Маскировка) — как на iOS и в ядре. Android оставался на голом
    // «noise»: правку от 26.07 сюда не перенесли, и поднятая с телефона сеть была
    // заметнее для DPI, чем ровно такая же с iPhone. Ради этого проект и делается,
    // так что расхождение здесь дороже, чем разница в скорости.
    var hostProtocol by mutableStateOf(prefs.getString("host_proto", null) ?: "noise-obfs")
    var hostPublic by mutableStateOf(prefs.getBoolean("host_public", true))
    var hostError by mutableStateOf<String?>(null)
    var myHostInfo by mutableStateOf<Host?>(null) // своя запись в каталоге (гости/IP/…)
    var hostStartedAt by mutableStateOf<Long?>(null)

    // недавние (для текущего координатора)
    var recent by mutableStateOf<List<String>>(emptyList())

    // недавние серверы (история координаторов)
    var serverHistory by mutableStateOf<List<String>>(loadList("server_history"))

    private var watchJob: Job? = null
    private var checkJob: Job? = null
    private var statusJob: Job? = null
    private var hostInfoJob: Job? = null
    private var applyJob: Job? = null

    /** Activity ставит: запросить VPN-согласие системы и запустить сервис. */
    var onStartVpn: ((Host, String) -> Unit)? = null

    private val hostToken: String
        get() = prefs.getString("host_token", null) ?: UUID.randomUUID().toString().also {
            prefs.edit().putString("host_token", it).apply()
        }

    private var started = false

    /** Подставить страну в имя хоста, если человек его ещё не менял.
     *  Зовётся, когда стал известен свой внешний IP: раньше страны просто нет. */
    private fun fillDefaultHostNameIfNeeded() {
        if (prefs.getString("host_name", null) != null) return
        val cc = GeoFlags.countryOf(myIp) ?: return
        val name = Locale("", cc).getDisplayCountry(Locale.getDefault())
        if (name.isNotEmpty() && name != cc) hostName = name
    }

    /**
     * Приложение вернулось на экран после фона.
     *
     * Пока оно было свёрнуто, система могла заморозить наши корутины, а сокет к
     * координатору за это время наверняка умер. Само по себе приложение об этом
     * НЕ УЗНАЁТ: serverOnline остаётся прежним, список выглядит живым, и полоса
     * «восстанавливаю связь» не появляется — хотя данные давно не актуальны.
     * Поэтому на возврате переспрашиваем связь и переустанавливаем подписку на
     * каталог: если связи нет, состояние честно станет «нет связи», и человек
     * увидит и приглушённый список, и дышащую полосу.
     */
    fun resumedFromBackground() {
        if (!started) return
        checkServer()
        startWatch()
    }

    fun start() {
        if (started) return
        started = true
        scope.launch(Dispatchers.IO) { GeoFlags.load(ctx) }   // база IP→страна в фоне
        loadRecent(); startWatch(); startStatus(); checkServer(); watchServer()
        // Хост-режим мог пережить пересоздание процесса (foreground-сервис).
        if (HostState.running) {
            hosting = true
            hostStartedAt = prefs.getLong("host_started_at", 0L).takeIf { it > 0 }
            startHostInfo()
        }
    }

    // ── персистентные списки (prefs) ──
    private fun loadList(key: String): List<String> =
        prefs.getString(key, null)?.split('\n')?.filter { it.isNotBlank() } ?: emptyList()

    private fun saveList(key: String, v: List<String>) {
        prefs.edit().putString(key, v.joinToString("\n")).apply()
    }

    fun addServerHistory(url: String) {
        val h = (listOf(url) + serverHistory.filter { it != url }).take(6)
        serverHistory = h; saveList("server_history", h)
    }

    fun removeServerHistory(url: String) {
        serverHistory = serverHistory.filter { it != url }; saveList("server_history", serverHistory)
    }

    /** Открыть по deep-link (bemyvpn://CODE или bemyvpn://connect?code=CODE). */
    fun openDeepLink(url: android.net.Uri) {
        var code = url.host ?: ""
        if (code == "connect") code = url.getQueryParameter("code") ?: ""
        if (code.isNotEmpty()) { tab = Tab.VPN; connectByCode(code) }
    }

    // ── каталог ──
    fun startWatch() {
        watchJob?.cancel()
        val coord = coordinator
        watchJob = scope.launch {
            var version = 0L
            while (isActive) {
                val (v, hs) = withContext(Dispatchers.IO) { parseEnvelope(Native.nativeListWatch(coord, version)) }
                if (!isActive || coord != coordinator) break
                if (v > 0) { version = v; hosts = hs; serverOnline = true }
                else { serverOnline = false; delay(3000) }
            }
        }
    }

    /**
     * Опрос реального статуса ядра — единственный источник правды о туннеле
     * (аналог applyTunnelStatus на iOS: там статусы шлёт система, здесь — ядро).
     */
    private fun startStatus() {
        statusJob?.cancel()
        statusJob = scope.launch {
            while (isActive) {
                val s = withContext(Dispatchers.IO) { try { Native.nativeVpnStatus() } catch (_: Throwable) { 0 } }
                applyCoreStatus(s)
                delay(700)
            }
        }
    }

    private var lastConnectTapAt = 0L
    private var lastStopAt = 0L

    private fun applyCoreStatus(s: Int) {
        when (s) {
            2 -> {
                // Сразу после «Стоп» ядро может ещё секунду отдавать 2 (BYE в пути,
                // сервис гасится) — не даём UI мигнуть обратно в «подключено».
                if (System.currentTimeMillis() - lastStopAt < 1500) return
                if (vpnState != 2) {
                    vpnState = 2
                    cancelQuick()   // подключились — прекращаем перебор кандидатов
                    // Успех-хаптик ТОЛЬКО на первое подключение (не на каждый
                    // авто-реконнект — иначе в машине телефон бы дребезжал).
                    if (connectedSince == null) { connectedSince = System.currentTimeMillis(); Haptics.success(ctx) }
                    // В «недавние» — ТОЛЬКО по факту реального подключения.
                    connectedTo?.let { addRecent(it) }
                }
            }
            1 -> vpnState = 1   // подключаюсь / авто-реконнект (reasserting)
            else -> {
                // 0/3. Грейс после тапа «Старт»: сервис ещё не дошёл до nativeConnect,
                // ядро пока отдаёт 0 — не сбрасываем оптимистичный статус UI.
                val grace = System.currentTimeMillis() - lastConnectTapAt < 6000
                if (!grace && vpnState != 0) {
                    // ПОЧЕМУ сеанс кончился. Хост, выключивший раздачу, — это не
                    // поломка: карточка просто гаснет, а словами объясняем, что
                    // произошло и что делать. Спрашиваем ядро именно здесь, на
                    // переходе в «выключено», и ровно один раз.
                    //
                    // Статус к этому моменту может быть уже 0, а не 3: сторож в
                    // BmvVpnService гасит сервис, едва увидев 3. Поэтому смотрим
                    // на причину, а не на статус — она держится до следующей
                    // попытки подключения.
                    val why = try { Native.nativeStopReason() } catch (_: Throwable) { 0 }
                    if (why == 1) showVpnError("Хост завершил раздачу — выберите другой", calm = true)
                    vpnState = 0; connectedTo = null; connectedSince = null
                }
            }
        }
    }

    private var serverWatchJob: Job? = null
    private val SERVER_POLL_MS = 1_000L

    /**
     * Держать пинг координатора живым — ВСЕГДА, как на десктопе и iOS.
     *
     * Раньше он замерялся только при запуске и по кнопке: человек смотрел на
     * цифру, снятую неизвестно когда, и не видел ни что связь пропала, ни что
     * вернулась. Особенно заметно после возврата из фона.
     *
     * Интервал ОДИН на все оболочки (SERVER_POLL_MS): на десктопе это давно
     * работало фоновым циклом, и делать на телефоне «то же, но по-другому»
     * значит плодить разное поведение там, где смысл один.
     *
     * Пауза берётся ПОСЛЕ проверки: на мёртвом сервере health висит до
     * таймаута, и запуск новой поверх незавершённой копил бы их.
     */
    fun watchServer() {
        serverWatchJob?.cancel()
        serverWatchJob = scope.launch {
            while (isActive) {
                delay(SERVER_POLL_MS)
                if (!isActive) return@launch
                checkServer()
            }
        }
    }

    fun checkServer() {
        checkJob?.cancel()
        val coord = coordinator
        checking = true
        checkJob = scope.launch {
            val t0 = System.currentTimeMillis()
            val ok = withContext(Dispatchers.IO) { try { Native.nativeHealth(coord) } catch (_: Throwable) { false } }
            val ms = (System.currentTimeMillis() - t0).toInt()
            val ip = if (ok) withContext(Dispatchers.IO) { try { Native.nativeMyIp(coord) } catch (_: Throwable) { "" } } else ""
            // Ответ ПРО ПРОШЛЫЙ сервер не должен затирать свежий результат:
            // health() к недоступному адресу висит на TCP-таймауте и возвращается,
            // когда пользователь уже переключился обратно на живой сервер.
            if (!isActive || coord != coordinator) return@launch
            // Пинг берём ЗАМЕРОМ, а не длительностью проверки: health читает флаг
            // «сокет жив» и возвращается мгновенно, поэтому «время вызова» пингом
            // никогда не было.
            val rtt = if (ok) withContext(Dispatchers.IO) {
                try { Native.nativeRttMs(coord) } catch (_: Throwable) { 0 }
            } else 0
            serverOnline = ok; ping = rtt; myIp = ip; checking = false
            fillDefaultHostNameIfNeeded()   // страна известна — можно назвать хост
            if (ok) addServerHistory(coord)
        }
    }

    fun saveCoordinator(url: String) {
        var u = url.trim()
        if (u.endsWith("/")) u = u.dropLast(1)
        if (!u.startsWith("http")) return
        coordinator = u
        prefs.edit().putString("coordinator", u).apply()
        hosts = emptyList(); resolvedHost = null
        loadRecent(); startWatch(); checkServer()
    }

    // ── гость ──
    fun hostById(id: String): Host? =
        hosts.firstOrNull { it.id == id } ?: resolvedHost?.takeIf { it.id == id }

    // ── Умный «Старт»: очередь кандидатов с фолбэком по таймауту ──
    /** Поводок для кандидата, ПОСЛЕ которого есть кого попробовать ещё. Внутри
     *  ядра на пробитие NAT отведено 12с — щедро, потому что двум мобильным NAT
     *  столько и нужно. Но пока в очереди ждут живые хосты, сидеть эти 12с у
     *  молчащего незачем: дешевле взять следующего. */
    private val quickAttemptMs = 5_000L
    /** Поводок для ПОСЛЕДНЕЙ попытки: запасных больше нет, поэтому даём пробитию
     *  доработать полностью. Прежние общие 8с были МЕНЬШЕ окна пробития, и у
     *  человека за строгим NAT «Старт» не срабатывал в принципе, сколько ни жми. */
    private val quickLastMs = 15_000L
    /** Сколько лучших кандидатов вообще пробуем. Без потолка перебор шёл бы по
     *  всему каталогу — при сотне свободных хостов это минуты ожидания. Не подошёл
     *  никто из первой пятёрки — проблема не в хостах. */
    private val quickMax = 5
    private var qcQueue = mutableListOf<Host>()   // оставшиеся кандидаты текущего «Старта»
    private var qcGen = 0                          // поколение попытки (инвалидирует старые таймеры)

    /** Код страны хоста (как в карточке: по IP, иначе по полю country). */
    private fun hostCountryCode(h: Host): String? =
        GeoFlags.countryOf(h.ip) ?: h.country.uppercase().ifEmpty { null }

    /**
     * Кандидаты для авто-подключения, УПОРЯДОЧЕННЫЕ: сначала хосты в ЧУЖОЙ
     * стране (VPN обычно нужен ради другой страны), затем — с бОльшим числом
     * свободных слотов. Страна клиента — регион устройства (оффлайн-эвристика).
     */
    private fun quickConnectCandidates(): List<Host> {
        val mine = Locale.getDefault().country.uppercase().ifEmpty { null }
        return hosts.filter { it.usable && !it.hasPassword && it.id != hostCode }
            .sortedWith(Comparator { a, b ->
                if (mine != null) {
                    val af = (hostCountryCode(a) ?: "") != mine
                    val bf = (hostCountryCode(b) ?: "") != mine
                    if (af != bf) return@Comparator if (af) -1 else 1   // чужая страна — раньше
                }
                (b.max - b.guests) - (a.max - a.guests)                 // больше свободных слотов
            })
            .take(quickMax)
    }

    /** «Старт»: пробуем кандидатов по очереди, пока один не подключится. */
    fun quickConnect() {
        if (vpnState != 0) return
        Haptics.tap(ctx)
        qcQueue = quickConnectCandidates().toMutableList()
        tryNextQuick()
    }

    /** Взять следующего кандидата и подключаться; на таймаут — дальше по очереди. */
    private fun tryNextQuick() {
        if (qcQueue.isEmpty()) {
            if (vpnState != 2) { vpnState = 0; connectedTo = null }
            return
        }
        val host = qcQueue.removeFirst()
        val isLast = qcQueue.isEmpty()
        qcGen += 1
        val gen = qcGen
        connect(host)
        // Не подключились за отведённое → гасим и берём следующего. Последнему
        // даём полный бюджет: запасных за ним нет, обрывать пробитие на полпути
        // уже незачем. Проверка поколения: успех/стоп/ручное подключение
        // увеличивают qcGen и глушат этот таймер, чтобы он не оборвал живое
        // соединение.
        scope.launch {
            delay(if (isLast) quickLastMs else quickAttemptMs)
            if (qcGen != gen || vpnState == 2) return@launch
            stopVpnService()
            tryNextQuick()
        }
    }

    /** Прекратить перебор (успех / стоп / ручной коннект): инвалидирует таймеры. */
    fun cancelQuick() { qcGen += 1; qcQueue.clear() }

    fun connect(host: Host, password: String = "") {
        // Свой же хост подключением не возьмёшь: пробитие пошло бы к самому себе,
        // и человек смотрел бы на «подключаюсь» до таймаута, не понимая почему.
        // На десктопе это подписано давно — здесь молчало.
        if (hosting && host.id == hostCode) {
            showVpnError("Это ваш собственный хост")
            return
        }
        vpnError = null
        connectedTo = host.id; vpnState = 1; expandedId = null
        lastConnectTapAt = System.currentTimeMillis()
        // Согласие системы на VPN + запуск сервиса — в Activity (нужен её контекст).
        onStartVpn?.invoke(host, password)
    }

    /** Activity: согласие на VPN не дали / сервис не стартовал → следующий кандидат. */
    fun connectFailed() {
        vpnState = 0; connectedTo = null
        tryNextQuick()
    }

    fun connectByCode(raw: String) {
        cancelQuick()   // ручной выбор перебивает авто-перебор «Старта»
        val code = raw.uppercase().trim()
        if (code.isEmpty()) return
        if (hosting && code == hostCode) {
            showVpnError("Это код вашего же хоста")
            return
        }
        hostById(code)?.let { h ->
            expandedId = code
            if (!h.hasPassword && h.usable) connect(h)
            return
        }
        val coord = coordinator
        scope.launch {
            val h = withContext(Dispatchers.IO) { parseHost(Native.nativeResolve(coord, code)) } ?: return@launch
            resolvedHost = h
            if (!h.hasPassword && h.usable) connect(h) else expandedId = h.id
        }
    }

    fun stop() {
        Haptics.tap(ctx)
        cancelQuick()   // пользователь остановил — гасим перебор кандидатов
        lastConnectTapAt = 0
        stopVpnService()
        connectedTo = null; vpnState = 0; connectedSince = null
    }

    private fun stopVpnService() {
        lastStopAt = System.currentTimeMillis()
        // Сервис синхронно шлёт BYE хосту (nativeStop) и закрывает TUN.
        ctx.startService(Intent(ctx, BmvVpnService::class.java).setAction(BmvVpnService.ACTION_STOP))
    }

    fun displayedHosts(): List<Host> {
        val out = hosts.filter { it.id != hostCode || !hosting }.toMutableList()  // прячем свой хост
        resolvedHost?.let { r -> if (out.none { it.id == r.id }) out.add(0, r) }
        return out
    }

    // ── хост ──
    fun ensureHostCode(done: (() -> Unit)? = null) {
        if (hostCode.isNotEmpty()) { done?.invoke(); return }
        val coord = coordinator
        scope.launch {
            val raw = withContext(Dispatchers.IO) { try { Native.nativeNewCode(coord) } catch (_: Throwable) { "" } }
            val parts = raw.split('|', limit = 2)
            if (parts[0].isNotEmpty()) setHostCode(parts[0], parts.getOrElse(1) { "" })
            done?.invoke()
        }
    }

    private fun setHostCode(code: String, sig: String) {
        hostCode = code; hostSig = sig
        prefs.edit().putString("host_code", code).putString("host_sig", sig).apply()
    }

    fun becomeHost() {
        persistHostSettings()
        starting = true; hostError = null
        ensureHostCode {
            val i = Intent(ctx, HostService::class.java)
                .putExtra(HostService.EXTRA_COORDINATOR, coordinator)
                .putExtra(HostService.EXTRA_ID, hostCode)
                .putExtra(HostService.EXTRA_TOKEN, hostToken)
                .putExtra(HostService.EXTRA_SIG, hostSig)
                .putExtra(HostService.EXTRA_NAME, hostName)
                .putExtra(HostService.EXTRA_MAX, hostMax)
                .putExtra(HostService.EXTRA_PASSWORD, hostPassword)
                .putExtra(HostService.EXTRA_PROTOCOL, hostProtocol)
                .putExtra(HostService.EXTRA_PUBLIC, hostPublic)
            HostState.result = null
            if (Build.VERSION.SDK_INT >= 26) ctx.startForegroundService(i) else ctx.startService(i)
            scope.launch {
                // Ждём вердикт сервиса (STUN+анонс — до десятков секунд).
                var waited = 0L
                while (HostState.result == null && waited < 60_000) { delay(200); waited += 200 }
                val res = HostState.result ?: ""
                starting = false
                when {
                    res == "!NAT" -> hostError = "Нет публичного адреса (вы за NAT) — раздача отсюда невозможна"
                    // Свежий код ядро берёт САМО; сюда доходит, только если и это
                    // не удалось (нет связи с сервером).
                    res == "!SIG" -> { setHostCode("", ""); hostError = "Сервер не подтвердил код — проверьте связь и повторите" }
                    res.isEmpty() || res.startsWith("!") -> hostError = "Не удалось включить раздачу"
                    else -> {
                        // Ответ — пара «код|подпись». Ядро могло вылечить протухшую
                        // подпись, взяв у сервера свежий код: сохраняем ОБЕ части,
                        // иначе в следующий раз уйдёт новый код со старой подписью.
                        val parts = res.split("|")
                        if (parts.size == 2 && parts[0].isNotEmpty() && parts[0] != hostCode) {
                            setHostCode(parts[0], parts[1])
                        }
                        hosting = true
                        hostStartedAt = System.currentTimeMillis()
                        startHostInfo()
                    }
                }
            }
        }
    }

    fun stopHost() {
        ctx.startService(Intent(ctx, HostService::class.java).setAction(HostService.ACTION_STOP))
        hostInfoJob?.cancel()
        hosting = false; starting = false; myHostInfo = null; hostStartedAt = null
    }

    /** Периодически тянем свою запись из каталога (гости/IP) — как на iOS. */
    private fun startHostInfo() {
        hostInfoJob?.cancel()
        val coord = coordinator; val code = hostCode
        hostInfoJob = scope.launch {
            while (isActive) {
                val h = withContext(Dispatchers.IO) { parseHost(try { Native.nativeResolve(coord, code) } catch (_: Throwable) { "" }) }
                if (!isActive) break
                myHostInfo = h
                delay(3000)
            }
        }
    }

    // Применить сразу (чипы протокола/видимости — один тап, флуда нет).
    fun applyHostNow() {
        persistHostSettings()
        if (!hosting) return
        val name = hostName; val maxg = hostMax; val pw = hostPassword; val proto = hostProtocol; val pub = hostPublic
        scope.launch(Dispatchers.IO) { try { Native.nativeHostUpdate(name, maxg, pw, proto, pub) } catch (_: Throwable) {} }
    }

    // Применить с задержкой (имя/пароль при печати) — чтобы не дёргать сервер на
    // каждую букву. Слайдер лимита применяется отдельно, только по отпусканию.
    fun applyHostDebounced() {
        persistHostSettings()
        if (!hosting) return
        applyJob?.cancel()
        applyJob = scope.launch {
            delay(800)
            applyHostNow()
        }
    }

    fun newHostCode() {
        val coord = coordinator
        val wasHosting = hosting
        if (wasHosting) stopHost()
        scope.launch {
            val raw = withContext(Dispatchers.IO) { try { Native.nativeNewCode(coord) } catch (_: Throwable) { "" } }
            val parts = raw.split('|', limit = 2)
            if (parts[0].isNotEmpty()) setHostCode(parts[0], parts.getOrElse(1) { "" })
            if (wasHosting) becomeHost()
        }
    }

    private fun persistHostSettings() {
        prefs.edit()
            .putString("host_name", hostName).putInt("host_max", hostMax)
            .putString("host_pw", hostPassword).putString("host_proto", hostProtocol)
            .putBoolean("host_public", hostPublic)
            .apply()
    }

    // ── недавние ──
    private val recentKey: String get() = "recent::$coordinator"
    private fun loadRecent() { recent = loadList(recentKey) }
    private fun addRecent(id: String) {
        val r = (listOf(id) + recent.filter { it != id }).take(6)
        recent = r; saveList(recentKey, r)
    }

    fun removeRecent(id: String) {
        recent = recent.filter { it != id }; saveList(recentKey, recent)
    }
}
