package org.bemyvpn

import android.content.Context

/**
 * ЕДИНСТВЕННЫЙ источник адреса главного сервера — assets/config.json. Меняешь
 * сервер — меняешь одно место. Никаких хардкод-дублей в коде: если конфиг не
 * прочитался (битый бандл — это ошибка сборки), вернём "" и подключение честно
 * не поднимется, а не уедет на «запасной» адрес. Пользовательский адрес (prefs)
 * всё равно перекрывает это.
 */
object Config {
    @Volatile private var cached: String? = null

    fun defaultCoordinator(ctx: Context): String {
        cached?.let { return it }
        val v = try {
            val json = ctx.assets.open("config.json").bufferedReader().use { it.readText() }
            org.json.JSONObject(json).optString("default_coordinator").trim().removeSuffix("/")
        } catch (_: Throwable) { "" }
        cached = v
        return v
    }
}
