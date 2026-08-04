package org.bemyvpn

import org.json.JSONArray
import org.json.JSONObject

/**
 * Карточка хоста из каталога (совпадает с JSON, что отдаёт мост; как в iOS Core.swift).
 *
 * Правила показа посчитаны В МОМЕНТ РАЗБОРА, а не при отрисовке: экран берёт
 * готовое поле и мост в теле перерисовки не зовёт. Раньше подпись собирали на
 * экране, а число для цвета выдирали из этой же подписи обратно — верный признак
 * того, что граница проведена не там.
 */
data class Host(
    val id: String,
    val name: String,
    val ip: String,
    val country: String,
    val guests: Int,
    val max: Int,
    val hasPassword: Boolean,
    val online: Boolean,
    val proto: String,
    val endpoints: String = "",
    // ── посчитано мостом при разборе ──
    /** Имя протокола по-человечески («Обычный» / «Маскировка» / …). */
    val protoName: String,
    /** Уровень защиты — варианты view::Protection; картинку выбирает экран. */
    val protection: Int,
    /** Годен для подключения (живой и есть место) — на этом гасится кнопка. */
    val usable: Boolean,
)

private fun hostOf(o: JSONObject): Host {
    val proto = o.optString("protocol")
    val online = o.optBoolean("online")
    val guests = o.optInt("guests")
    val max = o.optInt("max")
    return Host(
        id = o.optString("id"),
        name = o.optString("name"),
        ip = o.optString("ip"),
        country = o.optString("country"),
        guests = guests,
        max = max,
        hasPassword = o.optBoolean("hasPassword"),
        online = online,
        proto = proto,
        // Адреса для пробы отклика (через запятую) — до подключения их взять больше неоткуда.
        endpoints = o.optString("endpoints"),
        protoName = protoName(proto),
        protection = Native.nativeProtection(proto),
        usable = Native.nativeHostUsable(online, guests, max),
    )
}

/** {"version":N,"hosts":[...]} → (version, hosts); при ошибке (0, []). */
fun parseEnvelope(json: String): Pair<Long, List<Host>> = try {
    val o = JSONObject(json)
    val arr: JSONArray = o.optJSONArray("hosts") ?: JSONArray()
    o.optLong("version") to List(arr.length()) { hostOf(arr.getJSONObject(it)) }
} catch (_: Throwable) {
    0L to emptyList()
}

/** JSON-объект хоста → Host (null при пустой строке/ошибке). */
fun parseHost(json: String): Host? = try {
    if (json.isBlank()) null else hostOf(JSONObject(json))
} catch (_: Throwable) {
    null
}

/**
 * Отклик до хоста: подпись и уровень тревоги ВМЕСТЕ.
 *
 * Именно вместе, потому что порознь они и разъезжались: подпись брали из одного
 * места, цвет считали в другом — из этой же подписи, разобрав её обратно в
 * число. Оба поля приходят из справочника через мост.
 */
data class Ping(val text: String, val alarm: Int) {
    companion object {
        /** Первый замер ещё идёт: у экрана на это своя анимация ожидания. */
        val measuring = Ping("…", 3)

        /** Замер: null — хост не ответил (мост подписывает это прочерком). */
        fun of(ms: Int?): Ping {
            val v = ms ?: -1
            return Ping(Native.nativePingText(v), Native.nativePingAlarm(v))
        }
    }
}

/**
 * Имя протокола по-человечески — без крипто-жаргона, одним словом.
 *
 * КЛАССИФИКАЦИЮ даёт мост (`bmv_protection`): список идентификаторов здесь
 * больше не дублируется, и «пустой протокол — это шифрованный, а не голый»
 * теперь решает справочник, а не эта копия. Слова остались тут только потому,
 * что двери `bmv_proto_name` в мосте пока нет; появится — станет вызовом.
 */
fun protoName(p: String): String = when (Native.nativeProtection(p)) {
    0 -> "Обычный"
    1 -> "Маскировка"
    2 -> "Без шифра"
    // Незнакомое имя показываем как есть: врать «Без шифра» про неизвестный
    // протокол так же неверно, как врать про пустой.
    else -> p
}

/** Ненавязчивое пояснение к выбранному протоколу (по уровню защиты от моста). */
fun protoDesc(p: String): String = when (Native.nativeProtection(p)) {
    0 -> "Надёжное шифрование. Подходит почти всем — оставьте, если не уверены."
    1 -> "Прячет сам факт VPN: провайдер видит просто случайные данные. Чуть медленнее."
    2 -> "Шифрования нет — провайдер видит весь трафик. Только для сети, которой доверяете."
    else -> ""
}

/** Страна с флагом — определяется по IP локально (GeoFlags), как на iOS. */
fun countryLabel(h: Host): String {
    val cc = GeoFlags.countryOf(h.ip)
    if (cc != null) return "${GeoFlags.flagOfCc(cc)} $cc"
    if (h.ip.isNotEmpty()) return "🌍 ${h.ip}"
    return h.country.ifEmpty { "—" }
}

/** Флаг для «аватарки» слева в списке (🌍 если страна не определилась). */
fun hostFlag(h: Host): String = GeoFlags.countryOf(h.ip)?.let { GeoFlags.flagOfCc(it) } ?: "🌍"

/** Часы сеанса от отметки начала — правило и формат живут в справочнике (мост). */
fun uptimeText(sinceMs: Long?): String {
    if (sinceMs == null) return Native.nativeSessionClock(0)
    return Native.nativeSessionClock(((System.currentTimeMillis() - sinceMs) / 1000).coerceAtLeast(0))
}
