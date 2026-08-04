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
     * Имя протокола по-человечески — слово в пару к значку из nativeProtection.
     * Сами слова живут в справочнике; выписать их здесь значило бы завести
     * правило вторым местом — ровно так экран однажды объявил незашифрованным
     * хост, который шифрует.
     */
    external fun nativeProtoName(protocol: String): String

    /**
     * Уровень защиты по имени протокола — варианты view::Protection.
     * Что значит каждый номер, написано в справочнике; расшифровка здесь была бы
     * его второй копией.
     */
    external fun nativeProtection(protocol: String): Int

    /** Часы сеанса: MM:SS, после часа H:MM:SS. */
    external fun nativeSessionClock(seconds: Long): String

    /** Подпись пинга («24 мс» или прочерк). Отрицательное ms — ответа не было. */
    external fun nativePingText(ms: Int): String

    /** Тревожность пинга — варианты view::Alarm. Пороги живут в справочнике. */
    external fun nativePingAlarm(ms: Int): Int

    /** Годен ли хост (живой и есть место) — на этом гасится кнопка «Подключить». */
    external fun nativeHostUsable(online: Boolean, guests: Int, maxGuests: Int): Boolean

    /** Подпись состояния связи. online: 1 да · 0 нет · -1 ещё не знаем. */
    external fun nativeLinkText(online: Int): String

    /**
     * Тревожность состояния связи — варианты view::Alarm.
     * Обрыв там НЕ красный: сокет чинит супервизор сам, пугать нечем.
     */
    external fun nativeLinkAlarm(online: Int): Int

    /** Что написать на месте ПУСТОГО списка хостов: ждать связи или действовать. */
    external fun nativeEmptyDirectoryHint(online: Int): String

    /**
     * Состояние VPN числом — варианты view::Vpn. Номера и подписи к ним живут в
     * справочнике; повторять их здесь значило бы завести правило вторым местом.
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

    /**
     * СЫРОЙ статус ядра (0..3), как у bmv_vpn_status. Неблокирующая.
     * В состояние для показа его переводит nativeVpnKind — сами не толкуем.
     */
    external fun nativeVpnStatus(): Int

    /**
     * Почему сеанс кончился САМ: 0 — не кончался (идёт или выключили сами),
     * ненулевое — кончился сам. Неблокирующая.
     *
     * ЧТО именно случилось и какими словами это сказать, решает nativeVpnKind /
     * nativeVpnText: ненулевая причина — не обязательно поломка, и толковать её
     * на месте значит завести ещё одну копию правила.
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
