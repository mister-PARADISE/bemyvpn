package org.bemyvpn

/**
 * Мост к нативному ядру BeMyVPN (libbmv_android.so — тонкий JNI поверх bmv-ffi,
 * ОБЩЕГО с iOS). Поверхность зеркалит bmv_ffi.h один в один; блокирующие вызовы
 * (всё, кроме nativeVpnStatus/nativeNudge) — звать из фонового потока.
 */
object Native {
    init {
        System.loadLibrary("bmv_android")
    }

    // ── Сигналинг (координатор) ──────────────────────────────────────────────
    /** Живой каталог (long-poll): JSON {"version":N,"hosts":[...]}. since — из прошлого ответа. */
    external fun nativeListWatch(coordinator: String, since: Long): String

    /** Найти хост по коду (в т.ч. скрытый): JSON-объект хоста или "". */
    external fun nativeResolve(coordinator: String, code: String): String

    /** Новый код хоста от сервера как "CODE|SIG" ("" при ошибке). */
    external fun nativeNewCode(coordinator: String): String

    /** Свой внешний IP через координатор ("" при ошибке). */
    external fun nativeMyIp(coordinator: String): String

    /** Быстрая проверка связи: true = сервер жив. */
    external fun nativeHealth(coordinator: String): Boolean

    // ── Гость: подключение ───────────────────────────────────────────────────
    /** ФАЗА 1: пробитие NAT + рукопожатие БЕЗ TUN. true — канал готов. */
    external fun nativeConnect(coordinator: String, hostId: String, password: String, protocol: String): Boolean

    /** ФАЗА 2: качать пакеты через TUN-fd (от VpnService), с авто-реконнектом. */
    external fun nativeStartTunnel(fd: Int): Boolean

    /** Статус: 0=выкл 1=подключаюсь 2=подключено 3=ошибка. Неблокирующая. */
    external fun nativeVpnStatus(): Int

    /** Остановить VPN: синхронно шлёт хосту BYE на живом канале и гасит сессию. */
    external fun nativeStop()

    /** Смена сети (WiFi↔сотовая/вышка) — форсировать реконнект без падения TUN. */
    external fun nativeNudge()

    /** Отклик до хоста в мс, -1 = не ответил. Сессию на хосте не создаёт. */
    external fun nativeProbeRtt(coordinator: String, hostId: String, endpoints: String): Int

    // ── Хост-режим ───────────────────────────────────────────────────────────
    /** Стать хостом. Возвращает id, либо "!NAT" / "!SIG" / "". */
    external fun nativeHostStart(
        coordinator: String, hostId: String, token: String, codeSig: String,
        name: String, maxGuests: Int, password: String, protocol: String, public: Boolean,
    ): String

    external fun nativeHostStop()

    /** Сменить имя/лимит/пароль/протокол/видимость хоста НА ЛЕТУ. */
    external fun nativeHostUpdate(name: String, maxGuests: Int, password: String, protocol: String, public: Boolean)
}
