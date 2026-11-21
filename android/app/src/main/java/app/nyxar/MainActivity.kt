package app.nyxar

import android.Manifest
import android.app.Activity
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.net.VpnService
import android.os.Build
import android.os.Bundle
import android.text.InputType
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.animation.ObjectAnimator
import android.animation.ValueAnimator
import android.app.AlertDialog
import android.view.animation.DecelerateInterpolator
import android.widget.Button
import android.widget.FrameLayout
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
        val BG = Color.parseColor("#0F1020")
        val CARD = Color.parseColor("#181A2E")
        val TEXT = Color.parseColor("#E8E8F0")
        val MUTED = Color.parseColor("#8A8DA8")
        val ACCENT = Color.parseColor("#6D5BFF")
        val GREEN = Color.parseColor("#2FBF71")
        val RED = Color.parseColor("#FF6B6B")
        val ORANGE = Color.parseColor("#FFA94D")
        val IDLE = Color.parseColor("#2A2D4A")
    }

    private lateinit var prefs: Prefs
    private lateinit var power: TextView
    private lateinit var statusText: TextView
    private lateinit var errorText: TextView
    private lateinit var serverList: LinearLayout
    private lateinit var serverCount: TextView
    private lateinit var refreshBtn: TextView
    private lateinit var linkInput: EditText
    private lateinit var logBox: TextView
    private lateinit var logCard: LinearLayout

    private lateinit var pages: List<LinearLayout>
    private lateinit var tabs: List<TextView>
    private lateinit var scroll: ScrollView
    private lateinit var appsText: TextView
    private lateinit var quickName: TextView
    private lateinit var quickSub: TextView
    private var tab = 0
    private var listSig = ""
    private var pulse: ObjectAnimator? = null

    private var servers: List<Server> = emptyList()
    private var listError = ""
    private var refreshing = false

    private val stateListener: () -> Unit = { renderStatus(); renderServers() }

    // ───────────────────────── построение окна ─────────────────────────

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

    private fun card(vararg children: View): LinearLayout =
        LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            background = rounded(CARD)
            setPadding(dp(16), dp(14), dp(16), dp(14))
            layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.WRAP_CONTENT,
            ).apply { topMargin = dp(12) }
            children.forEach { addView(it) }
        }

    private fun button(label: String, onClick: () -> Unit) =
        Button(this).apply {
            text = label
            isAllCaps = false
            setTextColor(Color.WHITE)
            background = rounded(ACCENT, 12)
            setOnClickListener { onClick() }
            layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.WRAP_CONTENT,
            ).apply { topMargin = dp(10) }
        }

    private fun toggle(label: String, hint: String, checked: Boolean, onChange: (Boolean) -> Unit): View {
        val box = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(0, dp(6), 0, dp(6))
        }
        val sw = Switch(this).apply {
            text = label
            textSize = 15f
            setTextColor(TEXT)
            isChecked = checked
            setOnCheckedChangeListener { _, on -> onChange(on) }
        }
        box.addView(sw)
        box.addView(text(hint, 12f, MUTED))
        return box
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        prefs = Prefs(this)

        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(16), dp(20), dp(16), dp(24))
        }

        // заголовок
        val header = LinearLayout(this).apply { gravity = Gravity.CENTER_VERTICAL }
        header.addView(text("Nyxar", 24f, TEXT, true))
        header.addView(text("  ${BuildConfig.VERSION_NAME}", 13f, MUTED))
        root.addView(header)

        val pageHome = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        val pageServers = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; visibility = View.GONE }
        val pageSettings = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; visibility = View.GONE }
        pages = listOf(pageHome, pageServers, pageSettings)
        pages.forEach { root.addView(it) }

        // кнопка подключения и статус
        power = TextView(this).apply {
            gravity = Gravity.CENTER
            textSize = 16f
            setTextColor(Color.WHITE)
            setTypeface(typeface, Typeface.BOLD)
            layoutParams = LinearLayout.LayoutParams(dp(150), dp(150)).apply {
                gravity = Gravity.CENTER_HORIZONTAL
                topMargin = dp(8)
            }
            setOnClickListener { onPowerClick() }
        }
        statusText = text("", 16f, TEXT, true).apply { gravity = Gravity.CENTER }
        errorText = text("", 13f, RED).apply {
            gravity = Gravity.CENTER
            setPadding(0, dp(6), 0, 0)
        }
        pageHome.addView(
            card(power, statusText, errorText).also {
                (it.getChildAt(0).layoutParams as LinearLayout.LayoutParams).gravity = Gravity.CENTER_HORIZONTAL
            },
        )
        quickName = text("Сервер", 15f, TEXT, true)
        quickSub = text("", 12f, MUTED)
        pageHome.addView(
            card(quickName, quickSub).apply { setOnClickListener { showTab(1) } },
        )

        // серверы
        serverCount = text("", 13f, MUTED)
        refreshBtn = text("↻", 22f, ACCENT, true).apply {
            setPadding(dp(8), 0, dp(4), 0)
            setOnClickListener { refreshServers() }
        }
        val serversHeader = LinearLayout(this).apply { gravity = Gravity.CENTER_VERTICAL }
        serversHeader.addView(
            text("Серверы", 16f, TEXT, true),
            LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f),
        )
        serversHeader.addView(serverCount)
        serversHeader.addView(refreshBtn)
        serverList = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        pageServers.addView(card(serversHeader, serverList))

        // параметры
        pageSettings.addView(
            card(
                text("Параметры", 16f, TEXT, true),
                toggle("Россия напрямую", ".ru, .su, .рф и местные сервисы мимо VPN", prefs.bypassRu) {
                    prefs.bypassRu = it
                    restartIfActive()
                },
                toggle("Подробный лог", "для диагностики, файл быстро растёт", prefs.verbose) {
                    prefs.verbose = it
                    restartIfActive()
                },
            ),
        )

        // ссылка
        linkInput = EditText(this).apply {
            setText(prefs.link)
            hint = "vless://… или адрес подписки"
            setHintTextColor(MUTED)
            setTextColor(TEXT)
            textSize = 13f
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_MULTI_LINE or
                InputType.TYPE_TEXT_VARIATION_URI
            minLines = 2
            maxLines = 5
            background = rounded(Color.parseColor("#0F1020"), 10, Color.parseColor("#2A2D4A"), 1)
            setPadding(dp(10), dp(8), dp(10), dp(8))
        }
        pageServers.addView(
            card(
                text("Ссылка или подписка", 16f, TEXT, true),
                linkInput,
                button("Сохранить и подключить") { saveAndConnect() },
            ),
        )

        // программы мимо VPN
        appsText = text("", 13f, MUTED).apply { setPadding(0, dp(6), 0, 0) }
        pageSettings.addView(
            card(
                text("Программы мимо VPN", 16f, TEXT, true),
                appsText,
                button("Выбрать программы") { pickApps() },
                text("Выбранные программы ходят напрямую, остальные через VPN. Изменения применяются при следующем подключении.", 12f, MUTED)
                    .apply { setPadding(0, dp(8), 0, 0) },
            ),
        )

        // лог
        logBox = text("", 11f, MUTED).apply {
            typeface = Typeface.MONOSPACE
            setTextIsSelectable(true)
        }
        val logButtons = LinearLayout(this)
        logButtons.addView(
            Button(this).apply {
                text = "Обновить"
                isAllCaps = false
                setOnClickListener { showLog() }
            },
        )
        logButtons.addView(
            Button(this).apply {
                text = "Копировать"
                isAllCaps = false
                setOnClickListener {
                    val cm = getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                    cm.setPrimaryClip(ClipData.newPlainText("Nyxar log", logBox.text))
                    Toast.makeText(this@MainActivity, "Скопировано", Toast.LENGTH_SHORT).show()
                }
            },
        )
        logCard = card(text("Лог ядра", 16f, TEXT, true), logButtons, logBox).apply {
            visibility = View.GONE
        }
        val logToggle = text("Показать лог", 13f, ACCENT).apply {
            gravity = Gravity.CENTER
            setPadding(0, dp(14), 0, dp(4))
            setOnClickListener {
                if (logCard.visibility == View.VISIBLE) {
                    logCard.visibility = View.GONE
                    text = "Показать лог"
                } else {
                    showLog()
                    logCard.visibility = View.VISIBLE
                    text = "Скрыть лог"
                }
            }
        }
        pageSettings.addView(logToggle)
        pageSettings.addView(logCard)

        scroll = ScrollView(this).apply {
            isFillViewport = true
            addView(root)
        }
        root.setPadding(dp(16), dp(20), dp(16), dp(96)) // место под нижнюю панель

        // нижняя панель вкладок
        val bar = LinearLayout(this).apply {
            background = rounded(CARD, 20, Color.parseColor("#2A2D4A"), 1)
            setPadding(dp(4), dp(4), dp(4), dp(4))
            elevation = dp(8).toFloat()
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
        setContentView(FrameLayout(this).apply {
            setBackgroundColor(BG)
            addView(scroll, FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))
            addView(
                bar,
                FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT, Gravity.BOTTOM)
                    .apply { setMargins(dp(16), 0, dp(16), dp(14)) },
            )
        })
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

    override fun onDestroy() {
        VpnState.unlisten(stateListener)
        super.onDestroy()
    }

    // ───────────────────────── отрисовка ─────────────────────────

    private fun renderStatus() {
        val st = VpnState.status
        val (label, color, status) = when (st) {
            Status.OFF -> Triple("Подключить", IDLE, "Отключено")
            Status.CONNECTING -> Triple("Подключение…", ORANGE, "Подключение…")
            Status.ON -> Triple("Отключить", GREEN, "Подключено: ${VpnState.serverName}")
            Status.ERROR -> Triple("Повторить", RED, "Ошибка")
        }
        power.text = label
        power.background = GradientDrawable().apply {
            shape = GradientDrawable.OVAL
            setColor(color)
        }
        statusText.text = status
        if (st == Status.CONNECTING) {
            if (pulse == null) {
                pulse = ObjectAnimator.ofFloat(power, View.ALPHA, 1f, 0.55f).apply {
                    duration = 700
                    repeatMode = ValueAnimator.REVERSE
                    repeatCount = ValueAnimator.INFINITE
                    start()
                }
            }
        } else {
            pulse?.cancel()
            pulse = null
            power.alpha = 1f
        }
        quickName.text = if (st == Status.ON) VpnState.serverName else "Сервер"
        quickSub.text = if (prefs.server == "auto") "выбирается по пингу · нажми, чтобы выбрать" else "выбран вручную · нажми, чтобы изменить"
        errorText.text = if (st == Status.ERROR) VpnState.error else ""
        errorText.visibility = if (errorText.text.isEmpty()) View.GONE else View.VISIBLE
    }

    private fun serverRow(key: String, name: String, ping: Int?, showPing: Boolean): View {
        val selected = prefs.server == key
        val live = VpnState.status == Status.ON && VpnState.serverKey == key
        val row = LinearLayout(this).apply {
            gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(12), dp(10), dp(12), dp(10))
            background = rounded(
                if (selected) Color.parseColor("#262A52") else Color.TRANSPARENT,
                10,
                if (selected) ACCENT else Color.TRANSPARENT,
                1,
            )
            layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.WRAP_CONTENT,
            ).apply { topMargin = dp(4) }
            setOnClickListener {
                prefs.server = key
                renderServers()
                restartIfActive()
            }
        }
        val dot = View(this).apply {
            background = GradientDrawable().apply {
                shape = GradientDrawable.OVAL
                setColor(if (live) GREEN else Color.TRANSPARENT)
            }
            layoutParams = LinearLayout.LayoutParams(dp(8), dp(8)).apply { rightMargin = dp(10) }
        }
        row.addView(dot)
        row.addView(text(name, 14f), LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
        if (showPing) {
            val color = when {
                ping == null -> RED
                ping < 120 -> GREEN
                ping < 300 -> ORANGE
                else -> RED
            }
            row.addView(text(if (ping == null) "—" else "$ping мс", 13f, color))
        }
        return row
    }

    private fun renderServers() {
        val sig = "${servers.map { it.key + it.ping }}|${prefs.server}|${VpnState.status}|${VpnState.serverKey}|$refreshing|$listError"
        if (sig == listSig) return
        listSig = sig
        serverList.removeAllViews()
        refreshBtn.alpha = if (refreshing) 0.4f else 1f
        if (servers.isEmpty()) {
            serverCount.text = ""
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
        serverCount.text = "${servers.size}"
        val best = servers.filter { it.ping != null }.minByOrNull { it.ping!! }
        val autoName = if (best != null) "Авто · ${best.name}" else "Авто"
        serverList.addView(serverRow("auto", autoName, null, false))
        // выбранный раньше сервер мог пропасть из подписки: тогда работает «Авто»
        servers.forEach { serverList.addView(serverRow(it.key, it.name, it.ping, true)) }
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
            t.background = if (idx == i) rounded(Color.parseColor("#262A52"), 16, ACCENT, 1) else null
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
            VpnState.set(Status.ERROR, error = "Вставь ссылку vless:// или адрес подписки.")
            renderServers()
            return
        }
        refreshServers()
        connect()
    }
}
