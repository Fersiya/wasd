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
}
