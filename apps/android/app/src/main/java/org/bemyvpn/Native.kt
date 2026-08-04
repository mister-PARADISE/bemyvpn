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

    // ── Правила показа (справочник bmv_common::view) ─────────────────────────
    // Чистые функции: ни сети, ни блокировок — можно звать прямо с UI-потока.
    // Уровни отдаются ЧИСЛОМ, а не цветом и не именем значка: наборы значков у
    // оболочек разные, общего имени у них нет.

    /**
     * Набранный человеком адрес координатора → пригодный для работы.
     * ПУСТАЯ СТРОКА — ОТКАЗ, о котором экран обязан сказать вслух
     * («bemyvpn.net» без схемы раньше просто НЕ СОХРАНЯЛСЯ, молча).
     */
    external fun nativeCoordinatorUrl(input: String): String

    /** Адрес координатора ДЛЯ ПОКАЗА — без схемы и без хвостового слэша. */
    external fun nativeDisplayCoordinator(url: String): String

    /**
     * Уровень защиты по имени протокола: 0 шифр · 1 маскировка · 2 без шифра ·
     * 3 неизвестно (номера — варианты view::Protection).
     */
    external fun nativeProtection(protocol: String): Int

    /** Часы сеанса: MM:SS, после часа H:MM:SS. */
    external fun nativeSessionClock(seconds: Long): String

    /** Подпись пинга («24 мс» или прочерк). Отрицательное ms — ответа не было. */
    external fun nativePingText(ms: Int): String

    /**
     * Тревожность пинга: 0 спокойно · 1 янтарь · 2 красный · 3 приглушённо
     * (номера — варианты view::Alarm). Пороги живут в справочнике.
     */
    external fun nativePingAlarm(ms: Int): Int

    /** Годен ли хост (живой и есть место) — на этом гасится кнопка «Подключить». */
    external fun nativeHostUsable(online: Boolean, guests: Int, maxGuests: Int): Boolean

    /** Подпись состояния связи. online: 1 да · 0 нет · -1 ещё не знаем. */
    external fun nativeLinkText(online: Int): String

    /**
     * Тревожность состояния связи (номера — варианты view::Alarm).
     * Обрыв — ЯНТАРЬ: сокет чинит супервизор сам, красным тут пугать нечем.
     */
    external fun nativeLinkAlarm(online: Int): Int

    /** Что написать на месте ПУСТОГО списка хостов: ждать связи или действовать. */
    external fun nativeEmptyDirectoryHint(online: Int): String

    /**
     * Состояние VPN числом: 0 выкл · 1 подключаюсь · 2 переподключение ·
     * 3 работает · 4 хост завершил раздачу · 5 связь потеряна (варианты view::Vpn).
     * На входе — ровно то, что отдают nativeVpnStatus и nativeStopReason, плюс
     * «сеанс уже был» (у экрана есть отметка начала).
     */
    external fun nativeVpnKind(status: Int, stopReason: Int, wasConnected: Boolean): Int

    /** Подпись состояния VPN. Аргументы те же, что у nativeVpnKind. */
    external fun nativeVpnText(status: Int, stopReason: Int, wasConnected: Boolean): String

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

    /** Круг до координатора в мс; 0 — ещё не мерили или связи нет. */
    external fun nativeRttMs(coordinator: String): Int

    // ── Гость: подключение ───────────────────────────────────────────────────
    /** ФАЗА 1: пробитие NAT + рукопожатие БЕЗ TUN. true — канал готов. */
    external fun nativeConnect(coordinator: String, hostId: String, password: String, protocol: String): Boolean

    /** ФАЗА 2: качать пакеты через TUN-fd (от VpnService), с авто-реконнектом. */
    external fun nativeStartTunnel(fd: Int): Boolean

    /** Статус: 0=выкл 1=подключаюсь 2=подключено 3=ошибка. Неблокирующая. */
    external fun nativeVpnStatus(): Int

    /**
     * Почему сеанс кончился САМ: 0 — не кончался (идёт или выключили сами),
     * 1 — ХОСТ ЗАВЕРШИЛ РАЗДАЧУ, 2 — связь с хостом потеряна. Неблокирующая.
     *
     * Единица — это НЕ ошибка: всё сработало правильно, хост просто выключил
     * раздачу, и человеку надо предложить другой хост.
     */
    external fun nativeStopReason(): Int

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
