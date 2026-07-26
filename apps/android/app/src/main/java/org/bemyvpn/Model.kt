package org.bemyvpn

import org.json.JSONArray
import org.json.JSONObject

/** Карточка хоста из каталога (совпадает с JSON, что отдаёт мост; как в iOS Core.swift). */
data class Host(
    val id: String,
    val name: String,
    val ip: String,
    val country: String,
    val guests: Int,
    val max: Int,
    val hasPassword: Boolean,
    val online: Boolean,
    val isPublic: Boolean,
    val proto: String,
) {
    val usable: Boolean get() = online && guests < max
}

private fun hostOf(o: JSONObject) = Host(
    id = o.optString("id"),
    name = o.optString("name"),
    ip = o.optString("ip"),
    country = o.optString("country"),
    guests = o.optInt("guests"),
    max = o.optInt("max"),
    hasPassword = o.optBoolean("hasPassword"),
    online = o.optBoolean("online"),
    isPublic = o.optBoolean("public"),
    proto = o.optString("protocol"),
)

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

/** Имя протокола по-человечески — без крипто-жаргона, одним словом. */
fun protoName(p: String): String = when (p) {
    "noise", "noise-aes" -> "Обычный"
    "noise-obfs" -> "Скрытный"
    "plain", "" -> "Без шифра"
    else -> p
}

/** Ненавязчивое пояснение к выбранному протоколу. */
fun protoDesc(p: String): String = when (p) {
    "noise", "noise-aes" -> "Надёжное шифрование. Подходит почти всем — оставьте, если не уверены."
    "noise-obfs" -> "Прячет сам факт VPN: провайдер видит просто случайные данные. Чуть медленнее."
    "plain", "" -> "Шифрования нет — провайдер видит весь трафик. Только для сети, которой доверяете."
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

/**
 * Часы сессии — тикают посекундно: MM:SS, после часа H:MM:SS.
 * Именно посекундно: блок перерисовывается раз в секунду, и «12 мин»,
 * застывшее на минуту, выглядело бы зависшим.
 */
fun uptimeText(sinceMs: Long?): String {
    if (sinceMs == null) return "00:00"
    val s = ((System.currentTimeMillis() - sinceMs) / 1000).coerceAtLeast(0)
    val h = s / 3600; val m = (s % 3600) / 60; val sec = s % 60
    return if (h > 0) String.format("%d:%02d:%02d", h, m, sec) else String.format("%02d:%02d", m, sec)
}
