package app.nyxar

import android.Manifest
import android.app.Activity
import android.app.AlertDialog
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.content.res.ColorStateList
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.net.VpnService
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.text.InputType
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.view.animation.DecelerateInterpolator
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.Switch
import android.widget.TextView
import android.widget.Toast

class MainActivity : Activity() {
    private companion object {
        const val REQ_VPN = 1
        const val REQ_NOTIFICATIONS = 2
        val BG = Color.parseColor("#0C0D1A")
        val CARD = Color.parseColor("#171932")
        val CARD_LINE = Color.parseColor("#262A4D")
        val SELECTED = Color.parseColor("#232656")
        val TEXT = Color.parseColor("#ECECF5")
        val MUTED = Color.parseColor("#8A8DA8")
        val ACCENT = Color.parseColor("#6D5BFF")
        val CYAN = Color.parseColor("#22D3EE")
        val GREEN = Color.parseColor("#3EE08F")
        val RED = Color.parseColor("#FF7B7B")
        val ORANGE = Color.parseColor("#FFB454")
    }

    private lateinit var prefs: Prefs
    private lateinit var power: PowerButton
    private lateinit var statusText: TextView
    private lateinit var statusSub: TextView
    private lateinit var errorText: TextView
    private lateinit var serverList: LinearLayout
    private lateinit var serverCount: TextView
    private lateinit var refreshBtn: TextView
    private lateinit var sortBtn: TextView
    private lateinit var srvSearch: EditText
    private lateinit var linkInput: EditText
    private lateinit var logBox: TextView
    private lateinit var logCard: LinearLayout

    private lateinit var pages: List<LinearLayout>
    private lateinit var tabs: List<TextView>
    private lateinit var scroll: ScrollView
    private lateinit var appsText: TextView
    private lateinit var tileServer: TextView
    private lateinit var tileTime: TextView
    private lateinit var usageTotal: TextView
    private lateinit var limitTrack: LinearLayout
    private lateinit var limitFill: View
    private lateinit var limitRest: View
    private lateinit var limitInput: EditText
    private lateinit var topBox: LinearLayout
    private var topSig = ""
    private var tab = 0
    private var listSig = ""

    private var servers: List<Server> = emptyList()
    private var listError = ""
    private var refreshing = false

    private val ui = Handler(Looper.getMainLooper())
    private val ticker = object : Runnable {
        override fun run() {
            renderTime()
            ui.postDelayed(this, 1000)
        }
    }

    private val stateListener: () -> Unit = { renderStatus(); renderServers() }

    // ───────────────────────── помощники ─────────────────────────

    private fun dp(v: Int) = (v * resources.displayMetrics.density).toInt()

    private fun rounded(color: Int, radius: Int = 16, stroke: Int = 0, strokeWidth: Int = 0) =
        GradientDrawable().apply {
            setColor(color)
            cornerRadius = dp(radius).toFloat()
            if (stroke != 0) setStroke(dp(strokeWidth), stroke)
        }

    private fun text(s: String, size: Float = 14f, color: Int = TEXT, bold: Boolean = false) =
        TextView(this).apply {
            text = s
            textSize = size
            setTextColor(color)
            if (bold) setTypeface(typeface, Typeface.BOLD)
        }

    private fun lp(top: Int = 0, w: Int = ViewGroup.LayoutParams.MATCH_PARENT) =
        LinearLayout.LayoutParams(w, ViewGroup.LayoutParams.WRAP_CONTENT).apply { topMargin = dp(top) }

    /** Подпись раздела над карточкой. */
    private fun section(s: String) = text(s.uppercase(), 11f, MUTED, true).apply {
        letterSpacing = 0.12f
        setPadding(dp(6), dp(20), 0, dp(8))
    }

    private fun card(vararg children: View): LinearLayout =
        LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            background = rounded(CARD, 20, CARD_LINE, 1)
            setPadding(dp(18), dp(16), dp(18), dp(16))
            layoutParams = lp()
            children.forEach { addView(it) }
        }

    private fun button(label: String, onClick: () -> Unit) =
        Button(this).apply {
            text = label
            isAllCaps = false
            textSize = 15f
            setTypeface(typeface, Typeface.BOLD)
            setTextColor(Color.WHITE)
            stateListAnimator = null
            background = GradientDrawable(GradientDrawable.Orientation.LEFT_RIGHT, intArrayOf(ACCENT, Color.parseColor("#4F8BFF"))).apply {
                cornerRadius = dp(14).toFloat()
            }
            setOnClickListener { onClick() }
            layoutParams = lp(14).apply { height = dp(50) }
        }

    private fun ghostButton(label: String, onClick: () -> Unit) =
        Button(this).apply {
            text = label
            isAllCaps = false
            textSize = 13f
            setTextColor(TEXT)
            stateListAnimator = null
            background = rounded(Color.TRANSPARENT, 12, CARD_LINE, 1)
            setOnClickListener { onClick() }
            layoutParams = LinearLayout.LayoutParams(0, dp(42), 1f).apply { rightMargin = dp(8) }
        }

    private fun toggle(label: String, hint: String, checked: Boolean, onChange: (Boolean) -> Unit): View {
        val row = LinearLayout(this).apply {
            gravity = Gravity.CENTER_VERTICAL
            setPadding(0, dp(8), 0, dp(8))
        }
        val col = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        col.addView(text(label, 15f, TEXT, true))
        col.addView(text(hint, 12f, MUTED).apply { setPadding(0, dp(2), dp(12), 0) })
        row.addView(col, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
        val states = arrayOf(intArrayOf(android.R.attr.state_checked), intArrayOf())
        row.addView(
            Switch(this).apply {
                isChecked = checked
                thumbTintList = ColorStateList(states, intArrayOf(Color.WHITE, Color.parseColor("#B8BAD0")))
                trackTintList = ColorStateList(states, intArrayOf(ACCENT, Color.parseColor("#2E3158")))
                setOnCheckedChangeListener { _, on -> onChange(on) }
            },
        )
        return row
    }

    private fun divider() = View(this).apply {
        setBackgroundColor(CARD_LINE)
        layoutParams = LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 1).apply {
            topMargin = dp(4); bottomMargin = dp(4)
        }
    }

    /** Плитка с короткой подписью и значением. */
    private fun tile(label: String, value: TextView, onClick: (() -> Unit)? = null): LinearLayout =
        LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            background = rounded(CARD, 18, CARD_LINE, 1)
            setPadding(dp(16), dp(14), dp(16), dp(14))
            addView(text(label.uppercase(), 10f, MUTED, true).apply { letterSpacing = 0.12f })
            addView(value.apply { setPadding(0, dp(6), 0, 0); maxLines = 1; ellipsize = android.text.TextUtils.TruncateAt.END })
            if (onClick != null) setOnClickListener { onClick() }
        }

    // ───────────────────────── построение окна ─────────────────────────

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        prefs = Prefs(this)
        window.statusBarColor = BG
        window.navigationBarColor = BG

        // заголовок
        val header = LinearLayout(this).apply {
            gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(22), dp(14), dp(22), dp(6))
        }
        header.addView(text("Nyxar", 26f, TEXT, true))
        header.addView(
            text("v${BuildConfig.VERSION_NAME}", 11f, MUTED).apply {
                background = rounded(CARD, 10, CARD_LINE, 1)
                setPadding(dp(8), dp(3), dp(8), dp(3))
            },
            LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT).apply {
                leftMargin = dp(10)
            },
        )

        val content = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(16), dp(4), dp(16), dp(24))
        }
        val pageHome = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        val pageServers = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; visibility = View.GONE }
        val pageSettings = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; visibility = View.GONE }
        pages = listOf(pageHome, pageServers, pageSettings)
        pages.forEach { content.addView(it) }

        // ── главная
        power = PowerButton(this).apply {
            setOnClickListener { onPowerClick() }
            layoutParams = LinearLayout.LayoutParams(dp(250), dp(250)).apply {
                gravity = Gravity.CENTER_HORIZONTAL
                topMargin = dp(12)
            }
        }
        statusText = text("", 24f, TEXT, true).apply { gravity = Gravity.CENTER }
        statusSub = text("", 14f, MUTED).apply { gravity = Gravity.CENTER; setPadding(dp(16), dp(4), dp(16), 0) }
        errorText = text("", 13f, RED).apply {
            gravity = Gravity.CENTER
            setPadding(dp(16), dp(10), dp(16), 0)
        }
        pageHome.addView(power)
        pageHome.addView(statusText, lp())
        pageHome.addView(statusSub, lp())
        pageHome.addView(errorText, lp())

        tileServer = text("", 15f, TEXT, true)
        tileTime = text("—", 15f, TEXT, true)
        val tiles = LinearLayout(this).apply { layoutParams = lp(28) }
        tiles.addView(
            tile("Сервер", tileServer) { showTab(1) },
            LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1.3f).apply { rightMargin = dp(10) },
        )
        tiles.addView(tile("Время", tileTime), LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
        pageHome.addView(tiles)

        // ── серверы
        serverCount = text("", 12f, MUTED).apply {
            background = rounded(Color.parseColor("#232656"), 10)
            setPadding(dp(9), dp(3), dp(9), dp(3))
        }
        refreshBtn = text("↻", 22f, ACCENT, true).apply {
            setPadding(dp(14), 0, dp(2), 0)
            setOnClickListener { refreshServers() }
        }
        val serversHeader = LinearLayout(this).apply {
            gravity = Gravity.CENTER_VERTICAL
            setPadding(0, 0, 0, dp(6))
        }
        serversHeader.addView(
            text("Серверы", 18f, TEXT, true),
            LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f),
        )
        sortBtn = text(if (prefs.sortPing) "По пингу" else "Как в подписке", 12f, ACCENT, true).apply {
            setPadding(dp(12), dp(4), dp(4), dp(4))
            setOnClickListener {
                prefs.sortPing = !prefs.sortPing
                text = if (prefs.sortPing) "По пингу" else "Как в подписке"
                listSig = ""
                renderServers()
            }
        }
        srvSearch = EditText(this).apply {
            hint = "Найти сервер: страна, город, имя"
            setHintTextColor(MUTED)
            setTextColor(TEXT)
            textSize = 13f
            maxLines = 1
            inputType = InputType.TYPE_CLASS_TEXT
            background = rounded(BG, 12, CARD_LINE, 1)
            setPadding(dp(12), dp(8), dp(12), dp(8))
            visibility = View.GONE
            layoutParams = lp(0).apply { bottomMargin = dp(6) }
            addTextChangedListener(object : android.text.TextWatcher {
                override fun beforeTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
                override fun onTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
                override fun afterTextChanged(s: android.text.Editable?) { listSig = ""; renderServers() }
            })
        }
        serversHeader.addView(serverCount)
        serversHeader.addView(sortBtn)
        serversHeader.addView(refreshBtn)
        serverList = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        pageServers.addView(card(serversHeader, srvSearch, serverList))

        linkInput = EditText(this).apply {
            setText(prefs.link)
            hint = "vless://, trojan://, hy2://, ss:// или подписка"
            setHintTextColor(MUTED)
            setTextColor(TEXT)
            textSize = 13f
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_MULTI_LINE or
                InputType.TYPE_TEXT_VARIATION_URI
            minLines = 2
            maxLines = 5
            background = rounded(BG, 12, CARD_LINE, 1)
            setPadding(dp(12), dp(10), dp(12), dp(10))
        }
        pageServers.addView(section("Ссылка или подписка"))
        pageServers.addView(card(linkInput, button("Сохранить и подключить") { saveAndConnect() }))

        // ── настройки
        pageSettings.addView(section("Соединение"))
        pageSettings.addView(
            card(
                toggle("Россия напрямую", ".ru, .su, .рф и местные сервисы идут мимо VPN", prefs.bypassRu) {
                    prefs.bypassRu = it
                    restartIfActive()
                },
                divider(),
                toggle("Разблокировать Google AI и Antigravity", "Gemini, AI Studio, ChatGPT, Claude всегда через VPN", prefs.aiUnlock) {
                    prefs.aiUnlock = it
                    restartIfActive()
                },
                divider(),
                toggle("Обход блокировок ТСПУ (Фрагментация)", "TLS ClientHello fragmentation против DPI", prefs.fragment) {
                    prefs.fragment = it
                    restartIfActive()
                },
                divider(),
                toggle("Блокировка трафика при обрыве (Kill Switch)", "Запрещает утечки в сеть при сбое или отключении туннеля", prefs.killSwitch) {
                    prefs.killSwitch = it
                    restartIfActive()
                },
                divider(),
                toggle("Подробный лог", "для диагностики, файл быстро растёт", prefs.verbose) {
                    prefs.verbose = it
                    restartIfActive()
                },
            ),
        )

        // экономия трафика
        usageTotal = text("0 Б", 15f, TEXT, true)
        limitFill = View(this).apply { background = rounded(ACCENT, 4) }
        limitRest = View(this)
        limitTrack = LinearLayout(this).apply {
            weightSum = 100f
            background = rounded(CARD_LINE, 4)
            visibility = View.GONE
            layoutParams = lp(8).apply { height = dp(6) }
            addView(limitFill, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.MATCH_PARENT, 0f))
            addView(limitRest, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.MATCH_PARENT, 100f))
        }
        limitInput = EditText(this).apply {
            hint = "лимит на месяц, ГБ (пусто — без лимита)"
            setHintTextColor(MUTED)
            setTextColor(TEXT)
            textSize = 13f
            inputType = InputType.TYPE_CLASS_NUMBER or InputType.TYPE_NUMBER_FLAG_DECIMAL
            maxLines = 1
            background = rounded(BG, 12, CARD_LINE, 1)
            setPadding(dp(12), dp(10), dp(12), dp(10))
            layoutParams = lp(10)
            if (prefs.limitMb > 0) setText((prefs.limitMb / 1024.0).toString().removeSuffix(".0"))
        }
        topBox = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        val usageRow = LinearLayout(this).apply { gravity = Gravity.CENTER_VERTICAL }
        usageRow.addView(text("За этот месяц", 13f, MUTED), LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
        usageRow.addView(usageTotal)
        pageSettings.addView(section("Экономия трафика"))
        pageSettings.addView(
            card(
                toggle("Блокировать рекламу и трекеры", "рекламные сети и счётчики не загружаются: меньше трафика, быстрее страницы", prefs.saver) {
                    prefs.saver = it
                    restartIfActive()
                },
                divider(),
                usageRow,
                limitTrack,
                limitInput,
                button("Задать лимит") {
                    val gb = limitInput.text.toString().replace(',', '.').toDoubleOrNull() ?: 0.0
                    prefs.limitMb = if (gb > 0) Math.round(gb * 1024) else 0L
                    runCatching {
                        val t = UsageTracker(prefs)
                        t.warned = 0
                        t.save()
                    }
                    renderUsage()
                },
                topBox,
                ghostButton("Сбросить счётчик") { UsageTracker(prefs).reset(); topSig = ""; renderUsage() }
                    .apply { layoutParams = lp(10).apply { height = dp(42) } },
            ),
        )

        appsText = text("", 13f, MUTED).apply { setPadding(0, dp(2), 0, 0) }
        pageSettings.addView(section("Программы мимо VPN"))
        pageSettings.addView(
            card(
                appsText,
                button("Выбрать программы") { pickApps() },
                text("Выбранные программы ходят напрямую, остальные через VPN. Применится при следующем подключении.", 12f, MUTED)
                    .apply { setPadding(0, dp(10), 0, 0) },
            ),
        )

        logBox = text("", 11f, MUTED).apply {
            typeface = Typeface.MONOSPACE
            setTextIsSelectable(true)
            setPadding(0, dp(10), 0, 0)
        }
        val logButtons = LinearLayout(this).apply { layoutParams = lp(8) }
        logButtons.addView(ghostButton("Обновить") { showLog() })
        logButtons.addView(
            ghostButton("Копировать") {
                val cm = getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                cm.setPrimaryClip(ClipData.newPlainText("Nyxar log", logBox.text))
                Toast.makeText(this@MainActivity, "Скопировано", Toast.LENGTH_SHORT).show()
            },
        )
        logCard = card(logButtons, logBox).apply { visibility = View.GONE }
        val logToggle = text("Показать лог ядра", 13f, ACCENT).apply {
            gravity = Gravity.CENTER
            setPadding(0, dp(22), 0, dp(10))
            setOnClickListener {
                if (logCard.visibility == View.VISIBLE) {
                    logCard.visibility = View.GONE
                    text = "Показать лог ядра"
                } else {
                    showLog()
                    logCard.visibility = View.VISIBLE
                    text = "Скрыть лог ядра"
                }
            }
        }
        pageSettings.addView(logToggle)
        pageSettings.addView(logCard)

        scroll = ScrollView(this).apply {
            isFillViewport = true
            overScrollMode = View.OVER_SCROLL_NEVER
            addView(content)
        }

        // нижняя панель вкладок: под прокруткой, а не поверх неё
        val bar = LinearLayout(this).apply {
            background = rounded(CARD, 22, CARD_LINE, 1)
            setPadding(dp(5), dp(5), dp(5), dp(5))
        }
        tabs = listOf("Главная", "Серверы", "Настройки").mapIndexed { i, name ->
            TextView(this).apply {
                text = name
                gravity = Gravity.CENTER
                textSize = 13f
                setPadding(0, dp(12), 0, dp(12))
                setOnClickListener { showTab(i) }
                bar.addView(this, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
            }
        }

        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(BG)
            addView(header)
            addView(scroll, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f))
            addView(
                bar,
                LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT)
                    .apply { setMargins(dp(16), dp(8), dp(16), dp(12)) },
            )
        }
        // отступы под строку состояния и жестовую панель
        root.setOnApplyWindowInsetsListener { v, ins ->
            @Suppress("DEPRECATION")
            v.setPadding(ins.systemWindowInsetLeft, ins.systemWindowInsetTop, ins.systemWindowInsetRight, ins.systemWindowInsetBottom)
            ins
        }
        setContentView(root)
        root.requestApplyInsets()

        showTab(if (prefs.link.isBlank()) 1 else 0, animate = false)
        renderApps()

        if (Build.VERSION.SDK_INT >= 33) {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), REQ_NOTIFICATIONS)
        }

        VpnState.listen(stateListener)
        renderStatus()
        renderServers()
        if (prefs.link.isNotBlank()) refreshServers()
    }

    override fun onResume() {
        super.onResume()
        ui.post(ticker)
    }

    override fun onPause() {
        ui.removeCallbacks(ticker)
        super.onPause()
    }

    override fun onDestroy() {
        VpnState.unlisten(stateListener)
        super.onDestroy()
    }

    // ───────────────────────── отрисовка ─────────────────────────

    private fun fmtBytes(n: Long): String {
        if (n < 1024) return "$n Б"
        val u = arrayOf("КБ", "МБ", "ГБ", "ТБ")
        var v = n.toDouble()
        var i = -1
        do { v /= 1024; i++ } while (v >= 1024 && i < 3)
        return (if (v >= 100) "%.0f".format(v) else "%.1f".format(v)) + " " + u[i]
    }

    private fun appLabel(pkg: String): String =
        try { packageManager.getApplicationLabel(packageManager.getApplicationInfo(pkg, 0)).toString() } catch (_: Exception) { pkg }

    private fun renderUsage() {
        val u = UsageTracker(prefs)
        usageTotal.text = fmtBytes(u.total)
        val limit = prefs.limitMb * 1024 * 1024
        limitTrack.visibility = if (limit > 0) View.VISIBLE else View.GONE
        if (limit > 0) {
            val p = minOf(100f, u.total * 100f / limit)
            (limitFill.layoutParams as LinearLayout.LayoutParams).weight = p
            (limitRest.layoutParams as LinearLayout.LayoutParams).weight = 100f - p
            limitFill.background = rounded(if (p >= 80) ORANGE else ACCENT, 4)
            limitTrack.requestLayout()
        }
        val top = u.top(8)
        val sig = top.toString() + prefs.bypassApps
        if (sig == topSig) return
        topSig = sig
        topBox.removeAllViews()
        if (top.isEmpty()) {
            topBox.visibility = View.GONE
            return
        }
        topBox.visibility = View.VISIBLE
        topBox.addView(
            text("Больше всего трафика через VPN ушло у этих программ. Если приложение работает и без VPN, пусти его напрямую.", 12f, MUTED)
                .apply { setPadding(0, dp(14), 0, dp(4)) },
        )
        top.forEach { (pkg, bytes) ->
            val row = LinearLayout(this).apply {
                gravity = Gravity.CENTER_VERTICAL
                setPadding(0, dp(6), 0, dp(6))
            }
            row.addView(
                text(appLabel(pkg), 13f).apply { maxLines = 1; ellipsize = android.text.TextUtils.TruncateAt.END },
                LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f),
            )
            row.addView(text(fmtBytes(bytes), 12f, MUTED).apply { setPadding(dp(8), 0, dp(8), 0) })
            val canBypass = pkg != "—" && pkg != packageName
            if (canBypass) {
                val already = pkg in prefs.bypassApps
                row.addView(
                    text(if (already) "мимо VPN" else "Мимо VPN", 12f, if (already) MUTED else ACCENT, !already).apply {
                        background = rounded(Color.TRANSPARENT, 9, CARD_LINE, 1)
                        setPadding(dp(10), dp(4), dp(10), dp(4))
                        if (!already) setOnClickListener {
                            prefs.bypassApps = prefs.bypassApps + pkg
                            renderApps()
                            restartIfActive()
                            topSig = ""
                            renderUsage()
                        }
                    },
                )
            }
            topBox.addView(row)
        }
    }

    private fun renderTime() {
        renderUsage()
        if (VpnState.status != Status.ON) {
            tileTime.text = "—"
            return
        }
        val s = (SystemClock.elapsedRealtime() - VpnState.since) / 1000
        tileTime.text = if (s >= 3600) "%d:%02d:%02d".format(s / 3600, s / 60 % 60, s % 60) else "%02d:%02d".format(s / 60, s % 60)
    }

    private fun renderStatus() {
        val st = VpnState.status
        power.setStatus(st)
        when (st) {
            Status.OFF -> {
                statusText.text = "Не защищено"
                statusText.setTextColor(TEXT)
                statusSub.text = "Нажми на кнопку, чтобы подключиться"
            }
            Status.CONNECTING -> {
                statusText.text = "Подключаюсь…"
                statusText.setTextColor(ORANGE)
                statusSub.text = "Выбираю лучший сервер"
            }
            Status.ON -> {
                statusText.text = "Защищено"
                statusText.setTextColor(GREEN)
                statusSub.text = VpnState.serverName
            }
            Status.ERROR -> {
                statusText.text = "Не удалось"
                statusText.setTextColor(RED)
                statusSub.text = "Нажми, чтобы повторить"
            }
        }
        tileServer.text = when {
            st == Status.ON -> VpnState.serverName
            prefs.server == "auto" -> "Авто"
            else -> servers.firstOrNull { it.key == prefs.server }?.name ?: "Выбран вручную"
        }
        renderTime()
        errorText.text = if (st == Status.ERROR) VpnState.error else ""
        errorText.visibility = if (errorText.text.isEmpty()) View.GONE else View.VISIBLE
    }

    private fun serverRow(key: String, name: String, ping: Int?, showPing: Boolean): View {
        val selected = prefs.server == key
        val live = VpnState.status == Status.ON && VpnState.serverKey == key
        val row = LinearLayout(this).apply {
            gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(14), dp(13), dp(14), dp(13))
            background = rounded(
                if (selected) SELECTED else Color.TRANSPARENT,
                14,
                if (selected) ACCENT else Color.TRANSPARENT,
                1,
            )
            layoutParams = lp(4)
            setOnClickListener {
                prefs.server = key
                renderServers()
                renderStatus()
                restartIfActive()
            }
        }
        row.addView(
            View(this).apply {
                background = GradientDrawable().apply {
                    shape = GradientDrawable.OVAL
                    setColor(if (live) GREEN else if (selected) ACCENT else CARD_LINE)
                }
            },
            LinearLayout.LayoutParams(dp(9), dp(9)).apply { rightMargin = dp(12) },
        )
        row.addView(
            text(name, 14f, TEXT, selected).apply { maxLines = 1; ellipsize = android.text.TextUtils.TruncateAt.END },
            LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f),
        )
        if (showPing) {
            val fav = key in prefs.favorites
            row.addView(
                text(if (fav) "★" else "☆", 18f, if (fav) Color.parseColor("#FBBF24") else MUTED).apply {
                    setPadding(dp(6), 0, dp(10), 0)
                    setOnClickListener {
                        prefs.favorites = if (fav) prefs.favorites - key else prefs.favorites + key
                        listSig = ""
                        renderServers()
                    }
                },
            )
            val color = when {
                ping == null -> RED
                ping < 120 -> GREEN
                ping < 300 -> ORANGE
                else -> RED
            }
            row.addView(
                text(if (ping == null) "—" else "$ping мс", 12f, color, true).apply {
                    background = rounded(Color.argb(36, Color.red(color), Color.green(color), Color.blue(color)), 10)
                    setPadding(dp(9), dp(3), dp(9), dp(3))
                },
            )
        }
        return row
    }

    private fun renderServers() {
        val query = srvSearch.text.toString().trim().lowercase()
        val sig = "${servers.map { it.key + it.ping }}|${prefs.server}|${VpnState.status}|${VpnState.serverKey}|$refreshing|$listError|${prefs.favorites}|${prefs.sortPing}|$query"
        if (sig == listSig) return
        listSig = sig
        serverList.removeAllViews()
        refreshBtn.alpha = if (refreshing) 0.4f else 1f
        if (servers.isEmpty()) {
            serverCount.visibility = View.GONE
            serverList.addView(
                text(
                    when {
                        refreshing -> "Загружаю список…"
                        listError.isNotEmpty() -> listError
                        else -> "Добавь ссылку или подписку ниже"
                    },
                    13f,
                    if (listError.isNotEmpty() && !refreshing) RED else MUTED,
                ),
            )
            return
        }
        serverCount.visibility = View.VISIBLE
        serverCount.text = "${servers.size}"
        val best = servers.filter { it.ping != null }.minByOrNull { it.ping!! }
        val autoName = if (best != null) "Авто · ${best.name}" else "Авто"
        serverList.addView(serverRow("auto", autoName, null, false))
        // выбранный раньше сервер мог пропасть из подписки: тогда работает «Авто»
        srvSearch.visibility = if (servers.size > 8) View.VISIBLE else View.GONE
        val favs = prefs.favorites
        val shown = servers
            .filter { query.isEmpty() || it.name.lowercase().contains(query) }
            .sortedWith(
                compareBy<Server> { if (it.key in favs) 0 else 1 }
                    .thenBy { if (prefs.sortPing) (it.ping ?: Int.MAX_VALUE) else 0 },
            )
        shown.forEach { serverList.addView(serverRow(it.key, it.name, it.ping, true)) }
        if (shown.isEmpty()) serverList.addView(text("Ничего не найдено.", 13f, MUTED))
        // плавное появление строк по очереди
        for (i in 0 until serverList.childCount) {
            serverList.getChildAt(i).apply {
                alpha = 0f
                translationY = dp(8).toFloat()
                animate().alpha(1f).translationY(0f).setStartDelay((minOf(i, 14) * 20).toLong()).setDuration(260)
                    .setInterpolator(DecelerateInterpolator()).start()
            }
        }
    }

    // ───────────────────────── вкладки ─────────────────────────

    private fun showTab(i: Int, animate: Boolean = true) {
        tab = i
        pages.forEachIndexed { idx, p ->
            if (idx == i) {
                p.visibility = View.VISIBLE
                if (animate) {
                    p.alpha = 0f
                    p.translationY = dp(12).toFloat()
                    p.animate().alpha(1f).translationY(0f).setDuration(280).setInterpolator(DecelerateInterpolator()).start()
                }
            } else {
                p.visibility = View.GONE
            }
        }
        tabs.forEachIndexed { idx, t ->
            t.setTextColor(if (idx == i) TEXT else MUTED)
            t.setTypeface(null, if (idx == i) Typeface.BOLD else Typeface.NORMAL)
            t.background = if (idx == i) rounded(SELECTED, 17, ACCENT, 1) else null
        }
        scroll.scrollTo(0, 0)
    }

    // ───────────────────────── программы мимо VPN ─────────────────────────

    private fun renderApps() {
        val pm = packageManager
        val names = prefs.bypassApps.map { pkg ->
            try { pm.getApplicationLabel(pm.getApplicationInfo(pkg, 0)).toString() } catch (_: Exception) { pkg }
        }.sorted()
        appsText.text = if (names.isEmpty()) "Пока пусто: всё идёт через VPN." else names.joinToString(", ")
    }

    private fun pickApps() {
        Thread {
            val pm = packageManager
            val launcher = Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER)
            val apps = pm.queryIntentActivities(launcher, 0)
                .map { it.activityInfo.packageName to it.loadLabel(pm).toString() }
                .filter { it.first != packageName }
                .distinctBy { it.first }
                .sortedBy { it.second.lowercase() }
            runOnUiThread {
                if (isDestroyed) return@runOnUiThread
                val chosen = prefs.bypassApps.toMutableSet()
                val checked = BooleanArray(apps.size) { apps[it].first in chosen }
                AlertDialog.Builder(this, android.R.style.Theme_Material_Dialog_Alert)
                    .setTitle("Программы мимо VPN")
                    .setMultiChoiceItems(apps.map { it.second }.toTypedArray(), checked) { _, i, on ->
                        if (on) chosen.add(apps[i].first) else chosen.remove(apps[i].first)
                    }
                    .setPositiveButton("Готово") { _, _ ->
                        prefs.bypassApps = chosen
                        renderApps()
                        restartIfActive()
                    }
                    .setNegativeButton("Отмена", null)
                    .show()
            }
        }.start()
    }

    private fun showLog() {
        fun tail(f: java.io.File, n: Int): String {
            val raw = if (f.exists()) f.readText() else ""
            val clean = raw.replace(Regex("\u001B\\[[0-9;]*[A-Za-z]"), "")
            return clean.lines().filter { it.isNotBlank() }.takeLast(n).joinToString("\n")
        }
        val core = tail(NyxarVpnService.logFile(this), 50)
        val xray = tail(NyxarVpnService.xrayLogFile(this), 25)
        logBox.text = listOf(core, if (xray.isEmpty()) "" else "── xray ──\n$xray")
            .filter { it.isNotEmpty() }
            .joinToString("\n")
            .ifEmpty { "Лог пуст" }
    }

    // ───────────────────────── действия ─────────────────────────

    private fun refreshServers() {
        val link = prefs.link
        if (link.isBlank() || refreshing) return
        refreshing = true
        listError = ""
        renderServers()
        Thread {
            var list: List<Server> = emptyList()
            var err = ""
            try {
                list = Engine.loadServers(link)
                Engine.pingAll(list)
            } catch (e: Exception) {
                err = e.message ?: e.toString()
            }
            runOnUiThread {
                if (isDestroyed) return@runOnUiThread
                refreshing = false
                servers = list
                listError = err
                if (prefs.server != "auto" && list.isNotEmpty() && list.none { it.key == prefs.server }) {
                    prefs.server = "auto"
                }
                renderServers()
            }
        }.start()
    }

    private fun onPowerClick() {
        when (VpnState.status) {
            Status.ON, Status.CONNECTING -> stopVpn()
            else -> connect()
        }
    }

    private fun connect() {
        if (prefs.link.isBlank()) {
            VpnState.set(Status.ERROR, error = "Сначала добавь ссылку или подписку ниже.")
            return
        }
        val intent = VpnService.prepare(this)
        if (intent != null) {
            startActivityForResult(intent, REQ_VPN)
        } else {
            startVpn()
        }
    }

    @Deprecated("Deprecated in Java")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode == REQ_VPN) {
            if (resultCode == RESULT_OK) {
                startVpn()
            } else {
                VpnState.set(Status.ERROR, error = "Без разрешения VPN подключиться нельзя.")
            }
        }
    }

    private fun startVpn() {
        val i = Intent(this, NyxarVpnService::class.java).setAction(NyxarVpnService.ACTION_START)
        if (Build.VERSION.SDK_INT >= 26) startForegroundService(i) else startService(i)
    }

    private fun stopVpn() {
        startService(Intent(this, NyxarVpnService::class.java).setAction(NyxarVpnService.ACTION_STOP))
    }

    /** Смена сервера или параметров при включённом VPN: переподключаемся. */
    private fun restartIfActive() {
        if (VpnState.status == Status.ON || VpnState.status == Status.CONNECTING) startVpn()
    }

    private fun saveAndConnect() {
        val link = linkInput.text.toString().trim()
        prefs.link = link
        servers = emptyList()
        if (link.isEmpty()) {
            VpnState.set(Status.ERROR, error = "Вставь ссылку (vless, trojan, hy2) или адрес подписки.")
            renderServers()
            return
        }
        refreshServers()
        connect()
    }
}
