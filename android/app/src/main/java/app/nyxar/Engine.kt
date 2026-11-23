package app.nyxar

import android.util.Base64
import org.json.JSONArray
import org.json.JSONObject
import java.net.HttpURLConnection
import java.net.InetSocketAddress
import java.net.Socket
import java.net.URL
import java.net.URLDecoder
import java.util.concurrent.Callable
import java.util.concurrent.Executors

/** Ошибка с текстом, который можно показать пользователю. */
class NyxarException(message: String) : Exception(message)

data class Server(
    val name: String,
    val host: String,
    val port: Int,
    val link: String,
    var ping: Int? = null,
) {
    /**
     * Уникальный ключ: хэш ссылки без имени (#...). Имя может меняться, а host:port у разных
     * записей подписки часто совпадает, поэтому ключом host:port пользоваться нельзя.
     */
    val key: String
        get() {
            var h = 0xcbf29ce484222325UL // FNV-1a
            for (b in link.substringBefore('#').trim().toByteArray(Charsets.UTF_8)) {
                h = h xor b.toUByte().toULong()
                h *= 0x100000001b3UL
            }
            return "s" + h.toString(16).padStart(16, '0')
        }
}

/** Всё, что нужно сервису для запуска ядра. */
class Prepared(
    val server: Server,
    val config: JSONObject,
    /** Конфиг Xray и его локальный порт, если сервер работает через Xray (xhttp и др.). */
    val xrayConfig: JSONObject? = null,
    val xrayPort: Int = 0,
    /** Порт локального API ядра: по нему считаем трафик. */
    val apiPort: Int = 0,
)

object Engine {
    /** Домены, которые при включённой опции идут напрямую, мимо VPN. */
    private val RU_DIRECT = listOf(
        "ru", "su", "xn--p1ai", "yandex.net", "yandex.com", "yastatic.net", "vk.com", "vk.me",
        "vk-cdn.net", "vkuser.net", "userapi.com", "mycdn.me", "avito.st", "wbstatic.net",
        "2gis.com", "kinopoisk.dev",
    )

    /** Домены AI сервисов (Gemini, Antigravity, ChatGPT, Claude) для принудительного пропуска через VPN */
    val AI_DOMAINS = listOf(
        "generativelanguage.googleapis.com",
        "alkalimakersuite-pa.clients6.google.com",
        "aistudio.google.com",
        "deepmind.google",
        "gemini.google.com",
        "ai.google.dev",
        "proactivebackend-pa.googleapis.com",
        "openai.com",
        "chatgpt.com",
        "oaistatic.com",
        "oaiusercontent.com",
        "anthropic.com",
        "claude.ai",
    )

    /** Многие панели подписок отдают заглушку тем, кого не знают, поэтому пробуем несколько клиентов. */
    private val USER_AGENTS = listOf("v2rayNG/1.9.40", "Happ/2.0.0", "Hiddify/2.5.7", "v2rayN/7.4.0", "nyxar")

    @Volatile
    private var goodUa = 0

    // ───────────────────── разбор ссылок ─────────────────────

    private class ParsedLink(
        val scheme: String,
        val user: String,
        val host: String,
        val port: Int,
        val params: Map<String, String>,
        val name: String,
    )

    private class Vless(
        val uuid: String,
        val host: String,
        val port: Int,
        val params: Map<String, String>,
        val name: String,
    )

    private fun percentDecode(s: String): String {
        val out = java.io.ByteArrayOutputStream()
        var i = 0
        val bytes = s.toByteArray(Charsets.UTF_8)
        while (i < bytes.size) {
            val c = bytes[i].toInt()
            if (c == '%'.code && i + 2 < bytes.size) {
                val hex = String(bytes, i + 1, 2, Charsets.US_ASCII).toIntOrNull(16)
                if (hex != null) {
                    out.write(hex)
                    i += 3
                    continue
                }
            }
            out.write(bytes[i].toInt())
            i++
        }
        return String(out.toByteArray(), Charsets.UTF_8)
    }

    private fun formDecode(s: String): String =
        try {
            URLDecoder.decode(s, "UTF-8")
        } catch (_: IllegalArgumentException) {
            s
        }

    private fun parseLink(link: String): ParsedLink? {
        val l = link.trim()
        val scheme = when {
            l.startsWith("vless://") -> "vless"
            l.startsWith("trojan://") -> "trojan"
            l.startsWith("hysteria2://") -> "hysteria2"
            l.startsWith("hy2://") -> "hy2"
            else -> return null
        }
        val prefixLen = scheme.length + 3
        var rest = l.substring(prefixLen)
        val frag = rest.substringAfter('#', "")
        rest = rest.substringBefore('#')
        val query = rest.substringAfter('?', "")
        rest = rest.substringBefore('?')
        val at = rest.lastIndexOf('@')
        val user = if (at >= 0) percentDecode(rest.substring(0, at)) else ""
        val hostPort = if (at >= 0) rest.substring(at + 1).trimEnd('/') else rest.trimEnd('/')
        var host: String
        var port = 443
        if (hostPort.startsWith("[")) {
            val end = hostPort.indexOf(']')
            if (end < 0) return null
            host = hostPort.substring(1, end)
            val p = hostPort.substring(end + 1).removePrefix(":")
            if (p.isNotEmpty()) port = p.toIntOrNull() ?: return null
        } else {
            val c = hostPort.lastIndexOf(':')
            if (c >= 0) {
                host = hostPort.substring(0, c)
                port = hostPort.substring(c + 1).toIntOrNull() ?: return null
            } else {
                host = hostPort
            }
        }
        if (host.isEmpty()) return null
        val params = HashMap<String, String>()
        for (pair in query.split('&')) {
            if (pair.isEmpty()) continue
            params[formDecode(pair.substringBefore('='))] = formDecode(pair.substringAfter('=', ""))
        }
        return ParsedLink(scheme, user, host, port, params, percentDecode(frag).trim())
    }

    private fun parseVless(link: String): Vless? {
        val p = parseLink(link) ?: return null
        if (p.scheme != "vless") return null
        return Vless(p.user, p.host, p.port, p.params, p.name)
    }

    private fun decodeBase64Loose(s: String): String? {
        val clean = s.trim().filterNot { it.isWhitespace() }
        val rem = clean.length % 4
        val padded = if (rem > 0) clean + "=".repeat(4 - rem) else clean
        for (flags in intArrayOf(Base64.DEFAULT, Base64.URL_SAFE, Base64.NO_WRAP)) {
            try {
                val bytes = Base64.decode(padded, flags)
                val str = String(bytes, Charsets.UTF_8)
                if (str.isNotEmpty()) return str
            } catch (_: Exception) {}
        }
        return null
    }

    fun parseShadowsocks(link: String): JSONObject {
        val l = link.trim()
        if (!l.startsWith("ss://")) throw NyxarException("Нужна ссылка ss://")
        val rest = l.substring(5)
        val body = rest.substringBefore('#')
        val mainPart = body.substringBefore('?')

        val at = mainPart.indexOf('@')
        val (userInfo, hostPort) = if (at >= 0) {
            val u = mainPart.substring(0, at)
            val hp = mainPart.substring(at + 1)
            val decodedUser = decodeBase64Loose(u) ?: u
            Pair(decodedUser, hp)
        } else {
            val decoded = decodeBase64Loose(mainPart) ?: throw NyxarException("Не удалось декодировать base64 в ss://")
            val dat = decoded.indexOf('@')
            if (dat < 0) throw NyxarException("В ссылке ss:// нет @")
            Pair(decoded.substring(0, dat), decoded.substring(dat + 1))
        }

        val colon = userInfo.indexOf(':')
        if (colon < 0) throw NyxarException("В ссылке ss:// нет метода или пароля (method:password)")
        val method = userInfo.substring(0, colon)
        val password = userInfo.substring(colon + 1)

        val cleanHp = hostPort.trimEnd('/')
        val (host, port) = if (cleanHp.startsWith("[")) {
            val end = cleanHp.indexOf(']')
            if (end < 0) throw NyxarException("Неверный IPv6 в ss://")
            val h = cleanHp.substring(1, end)
            val p = cleanHp.substring(end + 1).removePrefix(":").toIntOrNull() ?: 8388
            Pair(h, p)
        } else {
            val c = cleanHp.lastIndexOf(':')
            if (c >= 0) {
                Pair(cleanHp.substring(0, c), cleanHp.substring(c + 1).toIntOrNull() ?: 8388)
            } else {
                Pair(cleanHp, 8388)
            }
        }
        if (host.isEmpty()) throw NyxarException("В ссылке ss:// нет адреса сервера")

        return jo(
            "type" to "shadowsocks",
            "tag" to "proxy",
            "server" to host,
            "server_port" to port,
            "method" to method,
            "password" to password,
        )
    }

    fun parseServer(link: String): Server? {
        val l = link.trim()
        if (l.startsWith("ss://")) {
            val ob = runCatching { parseShadowsocks(l) }.getOrNull() ?: return null
            val host = ob.getString("server")
            val port = ob.getInt("server_port")
            val frag = l.substringAfter('#', "")
            val name = if (frag.isNotEmpty()) percentDecode(frag).trim() else host
            return Server(name.ifEmpty { host }, host, port, l)
        }
        val p = parseLink(link) ?: return null
        return Server(p.name.ifEmpty { p.host }, p.host, p.port, link.trim())
    }

    /** Запись-пустышка: информация о трафике/сроке, заглушка, адрес-заглушка. */
    private fun isDummy(s: Server): Boolean {
        val name = s.name.lowercase()
        val host = s.host.lowercase()
        val badHost = host == "localhost" || host == "0.0.0.0" || host == "::" || host == "::1" ||
            host.startsWith("127.") || host.startsWith("0.") || host.startsWith("10.") ||
            host.startsWith("192.168.") || host.startsWith("169.254.")
        val badUuid = s.link.contains("00000000-0000-0000-0000-000000000000")
        val infoName = listOf(
            "осталось", "истека", "истёк", "истек", "expire", "traffic left", "трафик закон",
            "подписка закончилась", "не поддерж", "not support", "unsupported", "update your app",
            "обновите приложение",
        ).any { name.contains(it) }
        return s.port <= 1 || badHost || badUuid || infoName
    }

    private fun decodeMaybeB64(text: String): String {
        if (text.contains("vless://") || text.contains("trojan://") || text.contains("hysteria2://") || text.contains("hy2://") || text.contains("ss://")) return text
        val compact = text.filterNot { it.isWhitespace() }
        for (flags in intArrayOf(Base64.DEFAULT, Base64.URL_SAFE)) {
            try {
                val s = String(Base64.decode(compact, flags), Charsets.UTF_8)
                if (s.contains("://")) return s
            } catch (_: IllegalArgumentException) {
            }
        }
        return text
    }

    private fun extractLinks(text: String): List<String> =
        decodeMaybeB64(text).lines().map { it.trim() }.filter {
            it.startsWith("vless://") || it.startsWith("trojan://") || it.startsWith("hysteria2://") || it.startsWith("hy2://") || it.startsWith("ss://")
        }

    private fun download(url: String, ua: String): String {
        val c = URL(url).openConnection() as HttpURLConnection
        try {
            c.connectTimeout = 15000
            c.readTimeout = 15000
            c.setRequestProperty("User-Agent", ua)
            val code = c.responseCode
            if (code != 200) throw NyxarException("Подписка ответила ошибкой: HTTP $code")
            return c.inputStream.bufferedReader().use { it.readText() }
        } finally {
            c.disconnect()
        }
    }

    /** Принимает vless://, trojan://, hysteria2://, ss://, несколько ссылок или URL подписки. */
    fun fetchLinks(src: String): List<String> {
        val s = src.trim()
        if (s.isEmpty()) throw NyxarException("Ссылка не задана: вставь её в поле ниже")
        if (!(s.startsWith("http://") || s.startsWith("https://"))) {
            val links = extractLinks(s)
            if (links.isEmpty()) throw NyxarException("Не нашёл ни одной рабочей ссылки (vless://, trojan://, hysteria2://, ss://)")
            return links
        }
        val start = goodUa % USER_AGENTS.size
        var lastErr = "Не нашёл ни одной рабочей ссылки (vless://, trojan://, hysteria2://)"
        var stubName: String? = null
        for (i in USER_AGENTS.indices) {
            val idx = (start + i) % USER_AGENTS.size
            val text = try {
                download(s, USER_AGENTS[idx])
            } catch (e: NyxarException) {
                lastErr = e.message ?: lastErr
                continue
            } catch (e: java.io.IOException) {
                throw NyxarException("Не удалось скачать подписку: ${e.message}")
            }
            val servers = extractLinks(text).mapNotNull { parseServer(it) }
            if (servers.any { !isDummy(it) }) {
                goodUa = idx
                return servers.map { it.link }
            }
            servers.firstOrNull()?.let { stubName = it.name }
        }
        val stub = stubName
        if (stub != null) {
            throw NyxarException(
                "Подписка вернула заглушку «$stub» вместо серверов. " +
                    "Открой ссылку подписки в браузере и проверь, что там список серверов.",
            )
        }
        throw NyxarException(lastErr)
    }

    fun loadServers(src: String): List<Server> {
        val seen = HashSet<String>()
        var dummy = 0
        val reasons = LinkedHashMap<String, Int>()
        val out = ArrayList<Server>()
        for (link in fetchLinks(src)) {
            val s = parseServer(link) ?: continue
            if (isDummy(s)) {
                dummy++
            } else if (linkError(s.link) != null) {
                val why = linkError(s.link) ?: "ошибка"
                reasons[why] = (reasons[why] ?: 0) + 1
            } else if (seen.add(s.key)) {
                out.add(s)
            }
        }
        if (out.isEmpty()) {
            val detail = reasons.entries.joinToString("\n") { "• ${it.key}: ${it.value}" }
            throw NyxarException(
                "Нет рабочих серверов. Служебных записей: $dummy." +
                    (if (detail.isNotEmpty()) "\nОтброшены:\n$detail" else ""),
            )
        }
        return out
    }

    // ───────────────────── пинг и выбор ─────────────────────

    private fun tcpPing(host: String, port: Int): Int? =
        try {
            val t = System.nanoTime()
            Socket().use { it.connect(InetSocketAddress(host, port), 2000) }
            ((System.nanoTime() - t) / 1_000_000).toInt()
        } catch (_: Exception) {
            null
        }

    /** Имя для SNI, если сервер работает поверх TLS или Reality (иначе рукопожатие не имеет смысла). */
    internal fun tlsName(link: String): String? {
        val p = parseLink(link) ?: return null
        val sni = when (p.scheme) {
            "vless" -> {
                if (p.params["security"] != "tls" && p.params["security"] != "reality") return null
                (p.params["sni"] ?: p.params["serverName"]).orEmpty().ifEmpty { p.host }
            }
            "trojan" -> {
                (p.params["sni"] ?: p.params["peer"] ?: p.params["host"]).orEmpty().ifEmpty { p.host }
            }
            else -> return null
        }
        return if (Regex("^[0-9.]+$").matches(sni) || sni.contains(':')) null else sni
    }

    private val trustAll: javax.net.ssl.SSLSocketFactory by lazy {
        val tm = object : javax.net.ssl.X509TrustManager {
            override fun checkClientTrusted(chain: Array<java.security.cert.X509Certificate>?, authType: String?) {}
            override fun checkServerTrusted(chain: Array<java.security.cert.X509Certificate>?, authType: String?) {}
            override fun getAcceptedIssuers(): Array<java.security.cert.X509Certificate> = emptyArray()
        }
        val ctx = javax.net.ssl.SSLContext.getInstance("TLS")
        ctx.init(null, arrayOf<javax.net.ssl.TrustManager>(tm), null)
        ctx.socketFactory
    }

    /** Время TLS-рукопожатия с нужным SNI: честнее простого TCP, потому что доходит до самого сервера. */
    private fun tlsPing(host: String, port: Int, sni: String): Int? =
        try {
            val t = System.nanoTime()
            Socket().use { raw ->
                raw.connect(InetSocketAddress(host, port), 2500)
                raw.soTimeout = 2500
                val ssl = trustAll.createSocket(raw, sni, port, false) as javax.net.ssl.SSLSocket
                val params = ssl.sslParameters
                params.serverNames = listOf(javax.net.ssl.SNIHostName(sni))
                ssl.sslParameters = params
                ssl.startHandshake()
            }
            ((System.nanoTime() - t) / 1_000_000).toInt()
        } catch (_: Exception) {
            null
        }

    /** UDP-проверка для QUIC / Hysteria 2: отправляем запрос версии QUIC (RFC 9000). */
    private fun quicPing(host: String, port: Int): Int? =
        try {
            val addr = InetSocketAddress(host, port)
            val sock = java.net.DatagramSocket()
            sock.soTimeout = 1500
            val t = System.nanoTime()
            val buf = ByteArray(1200)
            buf[0] = 0xc0.toByte() // Long header
            buf[5] = 8.toByte()    // DCID len
            System.arraycopy(byteArrayOf(1, 2, 3, 4, 5, 6, 7, 8), 0, buf, 6, 8)
            buf[14] = 8.toByte()   // SCID len
            System.arraycopy(byteArrayOf(8, 7, 6, 5, 4, 3, 2, 1), 0, buf, 15, 8)
            val packet = java.net.DatagramPacket(buf, buf.size, addr)
            sock.send(packet)
            val resp = java.net.DatagramPacket(ByteArray(1500), 1500)
            sock.receive(resp)
            sock.close()
            ((System.nanoTime() - t) / 1_000_000).toInt().coerceAtLeast(1)
        } catch (_: Exception) {
            null
        }

    private fun probe(s: Server): Int? {
        val p = parseLink(s.link)
        if (p?.scheme == "hysteria2" || p?.scheme == "hy2") {
            return quicPing(s.host, s.port)
        }
        val sni = tlsName(s.link) ?: return tcpPing(s.host, s.port)
        val a = tlsPing(s.host, s.port, sni)
        val b = tlsPing(s.host, s.port, sni)
        val best = listOfNotNull(a, b).minOrNull()
        // до Android 10 TLS 1.3 нет, а некоторые Reality-сайты его требуют: тогда хотя бы TCP
        return best ?: if (android.os.Build.VERSION.SDK_INT < 29) tcpPing(s.host, s.port) else null
    }

    /** Пингует все серверы параллельно (TLS-рукопожатие, где возможно, иначе TCP). */
    fun pingAll(servers: List<Server>) {
        val pool = Executors.newFixedThreadPool(16)
        try {
            val tasks = servers.map { s -> Callable { s.ping = probe(s) } }
            pool.invokeAll(tasks)
        } finally {
            pool.shutdown()
        }
    }

    /** "auto" или неизвестный ключ: сервер с лучшим пингом, иначе первый. */
    fun pickServer(servers: List<Server>, key: String): Server? {
        if (key != "auto") servers.firstOrNull { it.key == key }?.let { return it }
        return servers.minByOrNull { it.ping ?: Int.MAX_VALUE }
    }

    // ───────────────────── конфиг sing-box ─────────────────────

    private fun jo(vararg p: Pair<String, Any?>): JSONObject {
        val o = JSONObject()
        for ((k, v) in p) o.put(k, v)
        return o
    }

    private fun ja(items: Collection<Any?>): JSONArray {
        val a = JSONArray()
        for (i in items) a.put(i)
        return a
    }

    private fun vlessOutbound(link: String): JSONObject {
        val u = parseVless(link) ?: throw NyxarException("Неверная ссылка vless://")
        if (u.uuid.isEmpty()) throw NyxarException("В ссылке нет UUID")
        val q = u.params
        fun g(k: String) = q[k] ?: ""

        val enc = g("encryption")
        if (enc.isNotEmpty() && enc != "none") {
            throw NyxarException("Шифрование VLESS encryption не поддерживается ядром")
        }

        val out = jo(
            "type" to "vless",
            "tag" to "proxy",
            "server" to u.host,
            "server_port" to u.port,
            "uuid" to u.uuid,
            "packet_encoding" to "xudp",
        )
        if (g("flow").isNotEmpty()) out.put("flow", g("flow"))

        val sni = g("sni").ifEmpty { g("host").ifEmpty { u.host } }
        val fp = g("fp").ifEmpty { "chrome" }

        when (val sec = g("security")) {
            "reality" -> out.put(
                "tls",
                jo(
                    "enabled" to true,
                    "server_name" to sni,
                    "utls" to jo("enabled" to true, "fingerprint" to fp),
                    "reality" to jo(
                        "enabled" to true,
                        "public_key" to g("pbk"),
                        "short_id" to g("sid"),
                    ),
                ),
            )
            "tls" -> {
                val tls = jo(
                    "enabled" to true,
                    "server_name" to sni,
                    "utls" to jo("enabled" to true, "fingerprint" to fp),
                )
                if (g("alpn").isNotEmpty()) tls.put("alpn", ja(g("alpn").split(',')))
                if (g("allowInsecure") in listOf("1", "true") || g("insecure") in listOf("1", "true")) {
                    tls.put("insecure", true)
                }
                out.put("tls", tls)
            }
            "", "none" -> {}
            else -> throw NyxarException("Защита $sec не поддерживается")
        }

        val path = g("path").ifEmpty { "/" }
        when (val type = g("type")) {
            "", "tcp" -> if (g("headerType") == "http") {
                val t = jo("type" to "http", "method" to "GET", "path" to path)
                if (g("host").isNotEmpty()) t.put("host", ja(g("host").split(',')))
                out.put("transport", t)
            }
            "ws" -> {
                val t = jo("type" to "ws", "path" to path)
                if (g("host").isNotEmpty()) t.put("headers", jo("Host" to g("host")))
                out.put("transport", t)
            }
            "grpc" -> out.put("transport", jo("type" to "grpc", "service_name" to g("serviceName")))
            "httpupgrade" -> {
                val t = jo("type" to "httpupgrade", "path" to path)
                if (g("host").isNotEmpty()) t.put("host", g("host"))
                out.put("transport", t)
            }
            "h2", "http" -> {
                val t = jo("type" to "http", "path" to path)
                if (g("host").isNotEmpty()) t.put("host", ja(g("host").split(',')))
                out.put("transport", t)
            }
            else -> throw NyxarException("Транспорт $type не поддерживается ядром")
        }
        return out
    }

    private fun trojanOutbound(link: String): JSONObject {
        val u = parseLink(link) ?: throw NyxarException("Неверная ссылка trojan://")
        if (u.scheme != "trojan") throw NyxarException("Нужна ссылка trojan://")
        if (u.user.isEmpty()) throw NyxarException("В ссылке нет пароля Trojan")
        val q = u.params
        fun g(k: String) = q[k] ?: ""

        val sni = g("sni").ifEmpty { g("peer").ifEmpty { g("host").ifEmpty { u.host } } }
        val fp = g("fp").ifEmpty { "chrome" }

        val tls = jo(
            "enabled" to true,
            "server_name" to sni,
            "utls" to jo("enabled" to true, "fingerprint" to fp),
        )
        if (g("alpn").isNotEmpty()) tls.put("alpn", ja(g("alpn").split(',')))
        if (g("allowInsecure") in listOf("1", "true") || g("insecure") in listOf("1", "true")) {
            tls.put("insecure", true)
        }

        val out = jo(
            "type" to "trojan",
            "tag" to "proxy",
            "server" to u.host,
            "server_port" to u.port,
            "password" to u.user,
            "tls" to tls,
        )

        val path = g("path").ifEmpty { "/" }
        when (val type = g("type")) {
            "", "tcp" -> {}
            "ws" -> {
                val t = jo("type" to "ws", "path" to path)
                if (g("host").isNotEmpty()) t.put("headers", jo("Host" to g("host")))
                out.put("transport", t)
            }
            "grpc" -> out.put("transport", jo("type" to "grpc", "service_name" to g("serviceName")))
            "httpupgrade" -> {
                val t = jo("type" to "httpupgrade", "path" to path)
                if (g("host").isNotEmpty()) t.put("host", g("host"))
                out.put("transport", t)
            }
            else -> throw NyxarException("Транспорт $type не поддерживается для Trojan")
        }
        return out
    }

    private fun hysteria2Outbound(link: String): JSONObject {
        val u = parseLink(link) ?: throw NyxarException("Неверная ссылка hysteria2://")
        if (u.scheme != "hysteria2" && u.scheme != "hy2") throw NyxarException("Нужна ссылка hysteria2:// или hy2://")
        val q = u.params
        fun g(k: String) = q[k] ?: ""
        val password = u.user.ifEmpty { g("auth") }
        if (password.isEmpty()) throw NyxarException("В ссылке нет пароля Hysteria2")

        val sni = g("sni").ifEmpty { g("peer").ifEmpty { u.host } }
        val tls = jo(
            "enabled" to true,
            "server_name" to sni,
        )
        if (g("insecure") in listOf("1", "true")) tls.put("insecure", true)
        if (g("alpn").isNotEmpty()) tls.put("alpn", ja(g("alpn").split(',')))

        val out = jo(
            "type" to "hysteria2",
            "tag" to "proxy",
            "server" to u.host,
            "server_port" to u.port,
            "password" to password,
            "tls" to tls,
        )
        if (g("obfs").isNotEmpty()) {
            out.put("obfs", jo("type" to g("obfs"), "password" to g("obfs-password")))
        }
        return out
    }

    fun nativeOutbound(link: String): JSONObject {
        val l = link.trim()
        return when {
            l.startsWith("vless://") -> vlessOutbound(l)
            l.startsWith("trojan://") -> trojanOutbound(l)
            l.startsWith("hysteria2://") || l.startsWith("hy2://") -> hysteria2Outbound(l)
            l.startsWith("ss://") -> parseShadowsocks(l)
            else -> throw NyxarException("Неподдерживаемый протокол ссылки")
        }
    }

    // ───────────────────── Xray (xhttp и др.) ─────────────────────
    // sing-box не умеет xhttp и VLESS encryption. Такие серверы обслуживает Xray-core:
    // sing-box (TUN) -> socks 127.0.0.1:порт -> xray -> сервер.

    /** Исходящее соединение для Xray; addr подменяет адрес сервера уже найденным IP. */
    fun xrayOutbound(link: String, addr: String? = null): JSONObject {
        val u = parseVless(link) ?: throw NyxarException("Неверная ссылка vless://")
        if (u.uuid.isEmpty()) throw NyxarException("В ссылке нет UUID")
        val q = u.params
        fun g(k: String) = q[k] ?: ""

        val user = jo("id" to u.uuid, "encryption" to g("encryption").ifEmpty { "none" })
        if (g("flow").isNotEmpty()) user.put("flow", g("flow"))

        val sni = g("sni").ifEmpty { g("host").ifEmpty { u.host } }
        val fp = g("fp").ifEmpty { "chrome" }
        val path = g("path").ifEmpty { "/" }
        val hostHdr = g("host").ifEmpty { u.host }

        val net = when (val t = g("type")) {
            "", "tcp" -> "tcp"
            "ws" -> "ws"
            "grpc" -> "grpc"
            "httpupgrade" -> "httpupgrade"
            "xhttp", "splithttp" -> "xhttp"
            else -> throw NyxarException("Транспорт $t не поддерживается")
        }
        if (net == "tcp" && g("headerType") == "http") {
            throw NyxarException("Транспорт tcp с http-обфускацией не поддерживается")
        }

        val ss = jo("network" to net)
        when (val sec = g("security")) {
            "reality" -> {
                ss.put("security", "reality")
                ss.put(
                    "realitySettings",
                    jo(
                        "serverName" to sni,
                        "fingerprint" to fp,
                        "publicKey" to g("pbk"),
                        "shortId" to g("sid"),
                        "spiderX" to g("spx"),
                    ),
                )
            }
            "tls" -> {
                ss.put("security", "tls")
                val tls = jo("serverName" to sni, "fingerprint" to fp)
                if (g("alpn").isNotEmpty()) tls.put("alpn", ja(g("alpn").split(',')))
                if (g("allowInsecure") in listOf("1", "true") || g("insecure") in listOf("1", "true")) {
                    tls.put("allowInsecure", true)
                }
                ss.put("tlsSettings", tls)
            }
            "", "none" -> ss.put("security", "none")
            else -> throw NyxarException("Защита $sec не поддерживается")
        }

        when (net) {
            "ws" -> ss.put("wsSettings", jo("path" to path, "host" to hostHdr))
            "grpc" -> ss.put(
                "grpcSettings",
                jo("serviceName" to g("serviceName"), "multiMode" to (g("mode") == "multi")),
            )
            "httpupgrade" -> ss.put("httpupgradeSettings", jo("path" to path, "host" to hostHdr))
            "xhttp" -> {
                val x = jo("path" to path, "host" to hostHdr, "mode" to g("mode").ifEmpty { "auto" })
                if (g("extra").isNotEmpty()) {
                    runCatching { JSONObject(g("extra")) }.getOrNull()?.let { x.put("extra", it) }
                }
                ss.put("xhttpSettings", x)
            }
        }

        return jo(
            "tag" to "proxy",
            "protocol" to "vless",
            "settings" to jo(
                "vnext" to ja(
                    listOf(
                        jo(
                            "address" to (addr ?: u.host),
                            "port" to u.port,
                            "users" to ja(listOf(user)),
                        ),
                    ),
                ),
            ),
            "streamSettings" to ss,
        )
    }

    /** null, если ссылку тянет sing-box или Xray; иначе причина отказа. */
    fun linkError(link: String): String? {
        if (runCatching { nativeOutbound(link) }.isSuccess) return null
        val viaXray = runCatching { xrayOutbound(link) }
        if (viaXray.isSuccess) return null
        return viaXray.exceptionOrNull()?.message ?: "ошибка"
    }

    fun needsXray(link: String): Boolean =
        runCatching { nativeOutbound(link) }.isFailure && runCatching { xrayOutbound(link) }.isSuccess

    fun xrayConfig(outbound: JSONObject, port: Int, verbose: Boolean): JSONObject = jo(
        "log" to jo("loglevel" to (if (verbose) "info" else "warning")),
        "inbounds" to ja(
            listOf(
                jo(
                    "tag" to "socks-in",
                    "listen" to "127.0.0.1",
                    "port" to port,
                    "protocol" to "socks",
                    "settings" to jo("auth" to "noauth", "udp" to true, "ip" to "127.0.0.1"),
                ),
            ),
        ),
        "outbounds" to ja(listOf(outbound, jo("tag" to "direct", "protocol" to "freedom"))),
    )

    private fun resolveServer(host: String): String {
        val all = try {
            java.net.InetAddress.getAllByName(host)
        } catch (e: java.io.IOException) {
            throw NyxarException("Не удалось определить адрес сервера $host: ${e.message}")
        }
        val pick = all.firstOrNull { it is java.net.Inet4Address } ?: all.first()
        return pick.hostAddress?.substringBefore('%') ?: throw NyxarException("Не удалось определить адрес $host")
    }

    private fun freePort(): Int =
        java.net.ServerSocket(0, 0, java.net.InetAddress.getByName("127.0.0.1")).use { it.localPort }

    fun buildConfig(
        outbound: JSONObject,
        bypassRu: Boolean,
        verbose: Boolean,
        logPath: String,
        saver: Boolean = false,
        apiPort: Int = 0,
        aiUnlock: Boolean = true,
        fragment: Boolean = true,
        killSwitch: Boolean = false,
        customDirect: Set<String> = emptySet(),
        customProxy: Set<String> = emptySet(),
    ): JSONObject {
        val dnsRules = arrayListOf<Any?>(
            jo("query_type" to ja(listOf("AAAA")), "action" to "reject", "method" to "default")
        )
        val routeRules = arrayListOf<Any?>(
            jo("ip_version" to 6, "action" to "reject", "method" to "default"),
            jo("action" to "sniff"),
            jo("protocol" to "dns", "action" to "hijack-dns"),
            jo("ip_is_private" to true, "action" to "route", "outbound" to "direct"),
        )
        if (bypassRu) {
            dnsRules.add(jo("domain_suffix" to ja(RU_DIRECT), "action" to "route", "server" to "local"))
            routeRules.add(jo("domain_suffix" to ja(RU_DIRECT), "action" to "route", "outbound" to "direct"))
        }
        if (saver) {
            // отказ на уровне DNS и сброс соединения: реклама и трекеры не загружаются
            dnsRules.add(0, jo("domain_suffix" to ja(Saver.ADS), "action" to "reject"))
            val at = routeRules.indexOfFirst { (it as? JSONObject)?.optString("action") == "hijack-dns" }
            val insertIdx = if (at >= 0) at + 1 else 2
            routeRules.add(insertIdx, jo("domain_suffix" to ja(Saver.ADS), "action" to "reject"))
        }
        if (aiUnlock) {
            dnsRules.add(jo("domain_suffix" to ja(AI_DOMAINS), "action" to "route", "server" to "remote"))
            routeRules.add(jo("domain_suffix" to ja(AI_DOMAINS), "action" to "route", "outbound" to "proxy"))
        }
        if (customDirect.isNotEmpty()) {
            val (domains, ips) = customDirect.map { it.trim() }.filter { it.isNotEmpty() }.partition { !it.contains('/') && !it.matches(Regex("^[0-9.:]+$")) }
            if (domains.isNotEmpty()) {
                dnsRules.add(jo("domain_suffix" to ja(domains), "action" to "route", "server" to "local"))
                routeRules.add(jo("domain_suffix" to ja(domains), "action" to "route", "outbound" to "direct"))
            }
            if (ips.isNotEmpty()) {
                routeRules.add(jo("ip_cidr" to ja(ips), "action" to "route", "outbound" to "direct"))
            }
        }
        if (customProxy.isNotEmpty()) {
            val (domains, ips) = customProxy.map { it.trim() }.filter { it.isNotEmpty() }.partition { !it.contains('/') && !it.matches(Regex("^[0-9.:]+$")) }
            if (domains.isNotEmpty()) {
                dnsRules.add(jo("domain_suffix" to ja(domains), "action" to "route", "server" to "remote"))
                routeRules.add(jo("domain_suffix" to ja(domains), "action" to "route", "outbound" to "proxy"))
            }
            if (ips.isNotEmpty()) {
                routeRules.add(jo("ip_cidr" to ja(ips), "action" to "route", "outbound" to "proxy"))
            }
        }
        if (killSwitch) {
            routeRules.add(jo("ip_is_private" to false, "action" to "route", "outbound" to "proxy"))
        }

        if (fragment) {
            val tls = outbound.optJSONObject("tls")
            if (tls != null) {
                tls.put("fragment", true)
                tls.put("record_fragment", true)
                tls.put("fragment_fallback_delay", "500ms")
            }
        }

        val cfg = jo(
            "log" to jo(
                "level" to (if (verbose) "info" else "warn"),
                "timestamp" to true,
                "output" to logPath,
            ),
            "dns" to jo(
                "strategy" to "ipv4_only",
                "servers" to ja(
                    listOf(
                        jo("tag" to "remote", "type" to "https", "server" to "1.1.1.1", "detour" to "proxy"),
                        jo("tag" to "local", "type" to "udp", "server" to "77.88.8.8"),
                    ),
                ),
                "rules" to ja(dnsRules),
                "final" to "remote",
            ),
            "inbounds" to ja(
                listOf(
                    jo(
                        "type" to "tun",
                        "tag" to "tun-in",
                        "address" to ja(listOf("172.19.0.1/30")),
                        "auto_route" to true,
                        "strict_route" to true,
                        "stack" to "mixed",
                    ),
                    jo(
                        "type" to "mixed",
                        "tag" to "mixed-in",
                        "listen" to "127.0.0.1",
                        "listen_port" to 2080,
                    ),
                ),
            ),
            "outbounds" to ja(listOf(outbound, jo("type" to "direct", "tag" to "direct"))),
            "route" to jo(
                "auto_detect_interface" to true,
                "default_domain_resolver" to "local",
                "final" to "proxy",
                "rules" to ja(routeRules),
            ),
        )
        if (apiPort != 0) {
            cfg.put("experimental", jo("clash_api" to jo("external_controller" to "127.0.0.1:$apiPort")))
        }
        return cfg
    }

    /** Скачивает подписку, выбирает сервер и собирает конфиг. Блокирующий вызов. */
    fun prepare(prefs: Prefs, logPath: String): Prepared {
        val servers = loadServers(prefs.link)
        val known = prefs.server != "auto" && servers.any { it.key == prefs.server }
        if (!known) pingAll(servers)
        val srv = pickServer(servers, prefs.server) ?: throw NyxarException("Нет доступных серверов")
        val api = freePort()
        if (needsXray(srv.link)) {
            val host = parseVless(srv.link)?.host ?: throw NyxarException("Неверная ссылка vless://")
            val port = freePort()
            val xcfg = xrayConfig(xrayOutbound(srv.link, resolveServer(host)), port, prefs.verbose)
            val socks = jo(
                "type" to "socks", "tag" to "proxy",
                "server" to "127.0.0.1", "server_port" to port, "version" to "5",
            )
            return Prepared(
                srv,
                buildConfig(
                    socks, prefs.bypassRu, prefs.verbose, logPath, prefs.saver, api,
                    prefs.aiUnlock, prefs.fragment, prefs.killSwitch, prefs.customDirect, prefs.customProxy,
                ),
                xcfg, port, api,
            )
        }
        val cfg = buildConfig(
            nativeOutbound(srv.link), prefs.bypassRu, prefs.verbose, logPath, prefs.saver, api,
            prefs.aiUnlock, prefs.fragment, prefs.killSwitch, prefs.customDirect, prefs.customProxy,
        )
        return Prepared(srv, cfg, apiPort = api)
    }
}
