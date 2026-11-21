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
)

object Engine {
    /** Домены, которые при включённой опции идут напрямую, мимо VPN. */
    private val RU_DIRECT = listOf(
        "ru", "su", "xn--p1ai", "yandex.net", "yandex.com", "yastatic.net", "vk.com", "vk.me",
        "vk-cdn.net", "vkuser.net", "userapi.com", "mycdn.me", "avito.st", "wbstatic.net",
        "2gis.com", "kinopoisk.dev",
    )

    /** Многие панели подписок отдают заглушку тем, кого не знают, поэтому пробуем несколько клиентов. */
    private val USER_AGENTS = listOf("v2rayNG/1.9.40", "Happ/2.0.0", "Hiddify/2.5.7", "v2rayN/7.4.0", "nyxar")

    @Volatile
    private var goodUa = 0

    // ───────────────────── разбор ссылок ─────────────────────

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

    private fun parseVless(link: String): Vless? {
        val l = link.trim()
        if (!l.startsWith("vless://")) return null
        var rest = l.removePrefix("vless://")
        val frag = rest.substringAfter('#', "")
        rest = rest.substringBefore('#')
        val query = rest.substringAfter('?', "")
        rest = rest.substringBefore('?')
        val at = rest.lastIndexOf('@')
        if (at < 0) return null
        val uuid = percentDecode(rest.substring(0, at))
        val hostPort = rest.substring(at + 1).trimEnd('/')
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
        return Vless(uuid, host, port, params, percentDecode(frag).trim())
    }

    fun parseServer(link: String): Server? {
        val v = parseVless(link) ?: return null
        return Server(v.name.ifEmpty { v.host }, v.host, v.port, link.trim())
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
        if (text.contains("vless://")) return text
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
        decodeMaybeB64(text).lines().map { it.trim() }.filter { it.startsWith("vless://") }

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

    /** Принимает vless://, несколько ссылок или URL подписки. */
    fun fetchLinks(src: String): List<String> {
        val s = src.trim()
        if (s.isEmpty()) throw NyxarException("Ссылка не задана: вставь её в поле ниже")
        if (!(s.startsWith("http://") || s.startsWith("https://"))) {
            val links = extractLinks(s)
            if (links.isEmpty()) throw NyxarException("Не нашёл ни одной ссылки vless://")
            return links
        }
        val start = goodUa % USER_AGENTS.size
        var lastErr = "Не нашёл ни одной ссылки vless://"
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
        val v = parseVless(link) ?: return null
        if (v.params["security"] != "tls" && v.params["security"] != "reality") return null
        val name = (v.params["sni"] ?: v.params["serverName"]).orEmpty().ifEmpty { v.host }
        // для IP-адреса без SNI рукопожатие смысла не имеет
        return if (Regex("^[0-9.]+$").matches(name) || name.contains(':')) null else name
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

    private fun probe(s: Server): Int? {
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
        if (runCatching { vlessOutbound(link) }.isSuccess) return null
        val viaXray = runCatching { xrayOutbound(link) }
        if (viaXray.isSuccess) return null
        return viaXray.exceptionOrNull()?.message ?: "ошибка"
    }

    fun needsXray(link: String): Boolean =
        runCatching { vlessOutbound(link) }.isFailure && runCatching { xrayOutbound(link) }.isSuccess

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

    fun buildConfig(outbound: JSONObject, bypassRu: Boolean, verbose: Boolean, logPath: String): JSONObject {
        val dnsRules = ArrayList<Any?>()
        val routeRules = arrayListOf<Any?>(
            jo("action" to "sniff"),
            jo("protocol" to "dns", "action" to "hijack-dns"),
            jo("ip_is_private" to true, "action" to "route", "outbound" to "direct"),
        )
        if (bypassRu) {
            dnsRules.add(jo("domain_suffix" to ja(RU_DIRECT), "action" to "route", "server" to "local"))
            routeRules.add(jo("domain_suffix" to ja(RU_DIRECT), "action" to "route", "outbound" to "direct"))
        }
        return jo(
            "log" to jo(
                "level" to (if (verbose) "info" else "warn"),
                "timestamp" to true,
                "output" to logPath,
            ),
            "dns" to jo(
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
                        "address" to ja(listOf("172.19.0.1/30", "fdfe:dcba:9876::1/126")),
                        "auto_route" to true,
                        "stack" to "mixed",
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
    }

    /** Скачивает подписку, выбирает сервер и собирает конфиг. Блокирующий вызов. */
    fun prepare(prefs: Prefs, logPath: String): Prepared {
        val servers = loadServers(prefs.link)
        val known = prefs.server != "auto" && servers.any { it.key == prefs.server }
        if (!known) pingAll(servers)
        val srv = pickServer(servers, prefs.server) ?: throw NyxarException("Нет доступных серверов")
        if (needsXray(srv.link)) {
            val host = parseVless(srv.link)?.host ?: throw NyxarException("Неверная ссылка vless://")
            val port = freePort()
            val xcfg = xrayConfig(xrayOutbound(srv.link, resolveServer(host)), port, prefs.verbose)
            val socks = jo(
                "type" to "socks", "tag" to "proxy",
                "server" to "127.0.0.1", "server_port" to port, "version" to "5",
            )
            return Prepared(srv, buildConfig(socks, prefs.bypassRu, prefs.verbose, logPath), xcfg, port)
        }
        val cfg = buildConfig(vlessOutbound(srv.link), prefs.bypassRu, prefs.verbose, logPath)
        return Prepared(srv, cfg)
    }
}
