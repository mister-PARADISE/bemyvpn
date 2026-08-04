package org.bemyvpn

import android.content.Context
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.zip.GZIPInputStream

/**
 * Флаг страны по IP — ОПРЕДЕЛЯЕТСЯ ЛОКАЛЬНО на устройстве, а не берётся из
 * самоотчёта хоста или главного сервера (которым доверять нельзя).
 *
 * База assets/ip2cc.dat — формат v2 (дельта-колоночный + gzip, ~0.45 МБ в APK
 * против 1.9 МБ у прежнего построчного):
 *   gzip( "BMV2" + n:u32 + deltas[n]:u32 + lens[n]:u32 + cc[n]:2×ASCII ), BE.
 *   start[i] = end[i-1] + delta[i] (wrap-around u32), end[i] = start[i] + len[i].
 * На загрузке раскодируется в массивы; поиск — двоичный по start (unsigned).
 * Грузится один раз в фоне; пока не готова — вызовы дают null (UI берёт фолбэк).
 */
object GeoFlags {
    @Volatile private var starts: IntArray? = null
    private var ends: IntArray = IntArray(0)
    private var cc: ByteArray = ByteArray(0)

    /** Загрузить базу в память (звать из фонового потока один раз). */
    fun load(ctx: Context) {
        if (starts != null) return
        try {
            val raw = GZIPInputStream(ctx.assets.open("ip2cc.dat")).use { it.readBytes() }
            val bb = ByteBuffer.wrap(raw).order(ByteOrder.BIG_ENDIAN)
            if (bb.int != 0x424D5632) return // "BMV2"
            val n = bb.int
            val s = IntArray(n)
            val e = IntArray(n)
            val deltas = IntArray(n) { bb.int }
            val lens = IntArray(n) { bb.int }
            var prevEnd = 0
            for (i in 0 until n) {
                s[i] = prevEnd + deltas[i] // переполнение Int = wrap-around u32, как в формате
                e[i] = s[i] + lens[i]
                prevEnd = e[i]
            }
            val c = ByteArray(n * 2)
            bb.get(c)
            ends = e; cc = c
            starts = s // публикация последней (volatile) — с этого момента ready
        } catch (_: Throwable) {
            // нет базы — не страшно, UI покажет глобус/фолбэк
        }
    }

    /** Код страны (ISO-2) по IPv4 «a.b.c.d» либо null. */
    fun countryOf(ip: String): String? {
        val s = starts ?: return null
        val key = ipv4ToLong(ip) ?: return null
        var lo = 0; var hi = s.size
        // ищем последнюю запись со start <= key (сравнение беззнаковое)
        while (lo < hi) {
            val mid = (lo + hi) ushr 1
            val start = s[mid].toLong() and 0xFFFFFFFFL
            if (start <= key) lo = mid + 1 else hi = mid
        }
        val i = lo - 1
        if (i < 0) return null
        val start = s[i].toLong() and 0xFFFFFFFFL
        val end = ends[i].toLong() and 0xFFFFFFFFL
        if (key < start || key > end) return null
        return "${cc[i * 2].toInt().toChar()}${cc[i * 2 + 1].toInt().toChar()}"
    }

    /** Эмодзи-флаг по коду страны (🌍, если код не двухбуквенный). */
    fun flagOfCc(country: String): String {
        val c = country.trim().uppercase()
        if (c.length != 2 || !c.all { it in 'A'..'Z' }) return "🌍"
        val a = 0x1F1E6
        return String(Character.toChars(a + (c[0] - 'A'))) + String(Character.toChars(a + (c[1] - 'A')))
    }

    private fun ipv4ToLong(ip: String): Long? {
        val p = ip.trim().split(".")
        if (p.size != 4) return null
        var v = 0L
        for (s in p) {
            val o = s.toIntOrNull() ?: return null
            if (o < 0 || o > 255) return null
            v = (v shl 8) or o.toLong()
        }
        return v
    }
}
