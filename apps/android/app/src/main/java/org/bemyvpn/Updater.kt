package org.bemyvpn

import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.core.content.FileProvider
import java.io.File
import java.net.HttpURLConnection
import java.net.URL

/**
 * Обновление приложения: спросить версию у GitHub, скачать APK, отдать системе.
 *
 * Доверие обеспечивает HTTPS: файл идёт с github.com, подменить его в пути
 * нельзя. Плюс Android сам не поставит APK, подписанный другим ключом, чем
 * установленный, — то есть чужую сборку он отвергнет даже если она как-то
 * подсунется.
 *
 * Полностью бесшумно на Android НЕ БЫВАЕТ: система всегда показывает свой
 * диалог установки для приложений не из магазина. Это ограничение платформы,
 * а не недоделка — мы лишь избавляем человека от похода на сайт и ручного
 * скачивания.
 */
object Updater {
    private const val REPO = "mister-PARADISE/bemyvpn"
    private const val ASSET = "bemyvpn-android-arm64.apk"
    /** Потолок размера: APK у нас ~4.5 МБ, 64 МБ с запасом и без риска съесть память. */
    private const val MAX_BYTES = 64L * 1024 * 1024

    /** Тег последнего релиза («v1.6») или null, если не удалось узнать. */
    fun latestTag(): String? = try {
        val body = httpGet(URL("https://api.github.com/repos/$REPO/releases/latest"), 256 * 1024)
        Regex("\"tag_name\"\\s*:\\s*\"([^\"]+)\"").find(String(body))?.groupValues?.get(1)
    } catch (_: Throwable) {
        null
    }

    /**
     * Новее ли `candidate` (без «v») текущей сборки.
     *
     * Сравнение ПО ЧИСЛАМ: строкой «1.10» оказалась бы меньше «1.9», и после
     * девятого выпуска обновление перестало бы предлагаться вовсе.
     */
    fun isNewer(candidate: String, current: String): Boolean {
        fun parts(v: String) = v.trimStart('v').split(".").take(3)
            .map { p -> p.takeWhile { it.isDigit() }.toIntOrNull() ?: 0 }
            .let { it + List(3 - it.size) { 0 } }
        val a = parts(candidate); val b = parts(current)
        for (i in 0..2) {
            if (a[i] != b[i]) return a[i] > b[i]
        }
        return false
    }

    /**
     * Скачать APK нужного релиза во внутреннюю папку приложения.
     *
     * Кладём в `cacheDir/updates`, а не в общее хранилище: не нужно разрешение
     * на запись, файл не мозолит глаза в «Загрузках» и убирается системой, если
     * места не хватит.
     */
    fun download(ctx: Context, tag: String): File {
        val dir = File(ctx.cacheDir, "updates").apply { mkdirs() }
        // Старые загрузки не копим — иначе кэш растёт с каждым обновлением.
        dir.listFiles()?.forEach { it.delete() }
        val out = File(dir, ASSET)
        val bytes = httpGet(URL("https://github.com/$REPO/releases/download/$tag/$ASSET"), MAX_BYTES)
        out.writeBytes(bytes)
        return out
    }

    /**
     * Отдать APK системному установщику. Дальше человек нажимает «Установить».
     *
     * Через FileProvider, а не file:// — начиная с Android 7 прямая ссылка на
     * файл роняет приложение (FileUriExposedException), а установщику ещё нужно
     * временное право на чтение.
     */
    fun install(ctx: Context, apk: File) {
        val uri: Uri = FileProvider.getUriForFile(ctx, "${ctx.packageName}.updates", apk)
        val intent = Intent(Intent.ACTION_VIEW).apply {
            setDataAndType(uri, "application/vnd.android.package-archive")
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        }
        ctx.startActivity(intent)
    }

    /** GET с ограничением размера и переходом по перенаправлениям (GitHub уводит на CDN). */
    private fun httpGet(url: URL, maxBytes: Long): ByteArray {
        var current = url
        repeat(5) {
            val c = (current.openConnection() as HttpURLConnection).apply {
                connectTimeout = 15_000
                readTimeout = 60_000
                instanceFollowRedirects = false // ведём сами: между http и https не прыгаем
                setRequestProperty("User-Agent", "bemyvpn")
            }
            try {
                val code = c.responseCode
                if (code in 300..399) {
                    val loc = c.getHeaderField("Location") ?: error("перенаправление без адреса")
                    val next = URL(current, loc)
                    require(next.protocol == "https") { "перенаправление не на HTTPS" }
                    current = next
                    return@repeat
                }
                require(code == 200) { "сервер ответил $code" }
                // Читаем ПО КУСКАМ и обрываем на переборе. `readBytes()` с
                // проверкой размера после — лимит, который срабатывает, когда
                // память уже съедена: на телефоне это просто падение приложения.
                val buf = java.io.ByteArrayOutputStream()
                val chunk = ByteArray(64 * 1024)
                c.inputStream.use { input ->
                    while (true) {
                        val n = input.read(chunk)
                        if (n < 0) break
                        require(buf.size() + n <= maxBytes) { "файл больше ожидаемого" }
                        buf.write(chunk, 0, n)
                    }
                }
                return buf.toByteArray()
            } finally {
                c.disconnect()
            }
        }
        error("слишком много перенаправлений")
    }
}
