package app.nyxar

import android.content.Context

class Prefs(context: Context) {
    private val sp = context.applicationContext.getSharedPreferences("nyxar", Context.MODE_PRIVATE)

    var link: String
        get() = sp.getString("link", "") ?: ""
        set(v) = sp.edit().putString("link", v).apply()

    /** "auto" или ключ сервера (Server.key). */
    var server: String
        get() = sp.getString("server", "auto") ?: "auto"
        set(v) = sp.edit().putString("server", v).apply()

    var bypassRu: Boolean
        get() = sp.getBoolean("bypass_ru", true)
        set(v) = sp.edit().putBoolean("bypass_ru", v).apply()

    var verbose: Boolean
        get() = sp.getBoolean("verbose", false)
        set(v) = sp.edit().putBoolean("verbose", v).apply()

    /** Пакеты приложений, которые идут мимо VPN. */
    var bypassApps: Set<String>
        get() = sp.getStringSet("bypass_apps", emptySet())?.toSet() ?: emptySet()
        set(v) = sp.edit().putStringSet("bypass_apps", v).apply()

    /** Блокировка рекламы и трекеров. */
    var saver: Boolean
        get() = sp.getBoolean("saver", true)
        set(v) = sp.edit().putBoolean("saver", v).apply()

    /** Месячный лимит трафика, МБ (0 — без лимита). */
    var limitMb: Long
        get() = sp.getLong("limit_mb", 0L)
        set(v) = sp.edit().putLong("limit_mb", v).apply()

    var usageJson: String
        get() = sp.getString("usage", "") ?: ""
        set(v) = sp.edit().putString("usage", v).apply()

    /** Избранные серверы (ключи): в списке идут первыми. */
    var favorites: Set<String>
        get() = sp.getStringSet("favorites", emptySet())?.toSet() ?: emptySet()
        set(v) = sp.edit().putStringSet("favorites", v).apply()

    /** Сортировать серверы по пингу (иначе порядок подписки). */
    var sortPing: Boolean
        get() = sp.getBoolean("sort_ping", true)
        set(v) = sp.edit().putBoolean("sort_ping", v).apply()
}
