package app.nyxar

import org.json.JSONObject
import java.util.Calendar
import java.util.TimeZone

/** Экономия трафика: список рекламных доменов и учёт трафика за месяц по программам. */
object Saver {
    val ADS = listOf(
        "doubleclick.net", "googlesyndication.com", "googleadservices.com", "adservice.google.com", "pagead2.googlesyndication.com",
        "google-analytics.com", "googletagmanager.com", "googletagservices.com", "admob.com", "adsense.com",
        "adnxs.com", "adsrvr.org", "adform.net", "adition.com", "advertising.com",
        "criteo.com", "criteo.net", "taboola.com", "outbrain.com", "revcontent.com",
        "mgid.com", "pubmatic.com", "rubiconproject.com", "openx.net", "casalemedia.com",
        "indexww.com", "smartadserver.com", "bidswitch.net", "contextweb.com", "amazon-adsystem.com",
        "media.net", "yieldmo.com", "sharethrough.com", "teads.tv", "scorecardresearch.com",
        "quantserve.com", "quantcount.com", "chartbeat.com", "chartbeat.net", "hotjar.com",
        "mouseflow.com", "fullstory.com", "crazyegg.com", "inspectlet.com", "luckyorange.com",
        "mixpanel.com", "segment.io", "segment.com", "amplitude.com", "heap.io",
        "heapanalytics.com", "branch.io", "adjust.com", "adjust.io", "appsflyer.com",
        "kochava.com", "singular.net", "tenjin.io", "app-measurement.com", "firebase-settings.crashlytics.com",
        "crashlytics.com", "flurry.com", "unityads.unity3d.com", "applovin.com", "applvn.com",
        "ironsrc.com", "supersonicads.com", "vungle.com", "chartboost.com", "inmobi.com",
        "mopub.com", "startapp.com", "adcolony.com", "tapjoy.com", "moatads.com",
        "adsafeprotected.com", "doubleverify.com", "serving-sys.com", "2mdn.net", "an.facebook.com",
        "analytics.tiktok.com", "ads.tiktok.com", "business-api.tiktok.com", "log.byteoversea.com", "ads.twitter.com",
        "static.ads-twitter.com", "analytics.twitter.com", "ads.linkedin.com", "px.ads.linkedin.com", "snap.licdn.com",
        "ads.pinterest.com", "ads.yahoo.com", "analytics.yahoo.com", "ads.reddit.com", "events.redditmedia.com",
        "mc.yandex.ru", "mc.yandex.com", "an.yandex.ru", "yandexadexchange.net", "adfox.yandex.ru",
        "ads.adfox.ru", "top-fwz1.mail.ru", "counter.yadro.ru", "mediametrics.ru", "adriver.ru",
        "ad.mail.ru", "tns-counter.ru", "adhigh.net", "buzzoola.com", "otm-r.com",
        "bidvol.com", "zedo.com", "bluekai.com", "demdex.net", "everesttech.net",
        "krxd.net", "exelator.com", "rlcdn.com", "tapad.com", "mathtag.com",
        "agkn.com", "addthis.com", "sharethis.com", "hs-analytics.net", "hs-scripts.com",
        "optimizely.com", "newrelic.com", "nr-data.net", "bugsnag.com", "sentry.io",
        "datadoghq.com", "statcounter.com", "histats.com", "clicky.com",
    )

    fun monthKey(): String {
        val c = Calendar.getInstance(TimeZone.getTimeZone("UTC"))
        return "%04d-%02d".format(c.get(Calendar.YEAR), c.get(Calendar.MONTH) + 1)
    }
}

/** Месячный счётчик. Хранится в Prefs, пишет сервис, читает окно. */
class UsageTracker(private val prefs: Prefs) {
    var month = ""
    var total = 0L
    var warned = 0
    val apps = HashMap<String, Long>()
    private var seen = HashMap<String, Pair<Long, Long>>()

    init { reload() }

    /** Перечитывает сохранённые значения (сброс или новый лимит из окна). Соединения не трогает. */
    fun reload() {
        apps.clear()
        month = ""; total = 0; warned = 0
        runCatching {
            val o = JSONObject(prefs.usageJson)
            month = o.optString("month")
            total = o.optLong("total")
            warned = o.optInt("warned")
            val a = o.optJSONObject("apps")
            a?.keys()?.forEach { apps[it] = a.getLong(it) }
        }
    }

    fun save() {
        val o = JSONObject()
        o.put("month", month)
        o.put("total", total)
        o.put("warned", warned)
        val a = JSONObject()
        apps.forEach { (k, v) -> a.put(k, v) }
        o.put("apps", a)
        prefs.usageJson = o.toString()
    }

    fun newSession() { seen = HashMap() }

    /** Разбирает ответ /connections и добавляет прирост трафика. */
    fun feed(body: String): Boolean {
        val m = Saver.monthKey()
        if (m != month) {
            month = m; total = 0; warned = 0; apps.clear()
        }
        val arr = JSONObject(body).optJSONArray("connections") ?: return false
        val now = HashMap<String, Pair<Long, Long>>()
        var added = 0L
        for (i in 0 until arr.length()) {
            val c = arr.getJSONObject(i)
            val id = c.optString("id")
            val up = c.optLong("upload")
            val down = c.optLong("download")
            val (pu, pd) = seen[id] ?: (0L to 0L)
            val delta = maxOf(0L, up - pu) + maxOf(0L, down - pd)
            if (delta > 0) {
                added += delta
                val name = shortName(c.optJSONObject("metadata")?.optString("processPath") ?: "")
                apps[name] = (apps[name] ?: 0L) + delta
            }
            now[id] = up to down
        }
        seen = now
        total += added
        return added > 0
    }

    fun top(n: Int): List<Pair<String, Long>> =
        apps.entries.sortedWith(compareByDescending<Map.Entry<String, Long>> { it.value }.thenBy { it.key })
            .take(n).map { it.key to it.value }

    /** Новый порог лимита (1 — 80%, 2 — 100%), каждый срабатывает раз в месяц. */
    fun checkLimit(limitMb: Long): Int? {
        if (limitMb <= 0) return null
        val limit = limitMb * 1024 * 1024
        val level = if (total >= limit) 2 else if (total * 5 >= limit * 4) 1 else 0
        if (level > warned) {
            warned = level
            return level
        }
        return null
    }

    fun reset() {
        total = 0; warned = 0; apps.clear(); seen = HashMap()
        save()
    }

    private fun shortName(p: String): String {
        val s = p.trim().substringBefore(" (").substringAfterLast('/').substringAfterLast('\\').trim()
        return s.ifEmpty { "—" }
    }
}
