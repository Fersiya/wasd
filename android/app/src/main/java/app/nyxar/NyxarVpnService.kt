package app.nyxar

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager.NameNotFoundException
import android.content.pm.ServiceInfo
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.net.VpnService
import android.os.Build
import android.os.ParcelFileDescriptor
import android.os.Process
import android.system.OsConstants
import android.util.Log
import io.nekohasekai.libbox.BridgeOptions
import io.nekohasekai.libbox.BridgeSession
import io.nekohasekai.libbox.CommandServer
import io.nekohasekai.libbox.CommandServerHandler
import io.nekohasekai.libbox.ConnectionOwner
import io.nekohasekai.libbox.InterfaceUpdateListener
import io.nekohasekai.libbox.Libbox
import io.nekohasekai.libbox.LocalDNSTransport
import io.nekohasekai.libbox.NeighborUpdateListener
import io.nekohasekai.libbox.NetworkInterfaceIterator
import io.nekohasekai.libbox.OverrideOptions
import io.nekohasekai.libbox.PlatformInterface
import io.nekohasekai.libbox.PlatformUser
import io.nekohasekai.libbox.ShellSession
import io.nekohasekai.libbox.StringIterator
import io.nekohasekai.libbox.SystemProxyStatus
import io.nekohasekai.libbox.TunOptions
import io.nekohasekai.libbox.WIFIState
import java.io.File
import java.net.HttpURLConnection
import java.net.InetSocketAddress
import java.net.InterfaceAddress
import java.net.URL
import java.util.concurrent.Executors
import io.nekohasekai.libbox.NetworkInterface as BoxInterface
import io.nekohasekai.libbox.Notification as BoxNotification

/**
 * VPN-сервис: поднимает TUN и отдаёт его ядру sing-box (libbox).
 * Запуск и остановка идут через одну очередь, чтобы не пересекаться.
 */
class NyxarVpnService : VpnService(), PlatformInterface, CommandServerHandler {
    companion object {
        const val ACTION_START = "app.nyxar.START"
        const val ACTION_STOP = "app.nyxar.STOP"
        private const val CHANNEL = "nyxar"
        private const val NOTIFICATION_ID = 1
        private const val TAG = "Nyxar"

        fun logFile(ctx: Context) = File(ctx.filesDir, "sing-box.log")

        fun xrayLogFile(ctx: Context) = File(ctx.filesDir, "xray.log")
    }

    private val worker = Executors.newSingleThreadExecutor()
    private var commandServer: CommandServer? = null
    private var tun: ParcelFileDescriptor? = null
    private var watcher: NetworkWatcher? = null
    private var xray: Process? = null

    // ───────────────────────── жизненный цикл ─────────────────────────

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        // после startForegroundService уведомление нужно показать в течение нескольких секунд
        showNotification("Подключение…")
        if (intent?.action == ACTION_STOP) {
            worker.execute {
                stopCore()
                VpnState.set(Status.OFF)
                stopForegroundCompat()
                stopSelf()
            }
        } else {
            worker.execute { startCore() }
        }
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        worker.execute { stopCore() }
        worker.shutdown()
        super.onDestroy()
    }

    override fun onRevoke() {
        // пользователь отключил VPN в системе
        worker.execute {
            stopCore()
            VpnState.set(Status.OFF)
            stopForegroundCompat()
            stopSelf()
        }
    }

    private fun startCore() {
        try {
            stopCore()
            VpnState.set(Status.CONNECTING)
            val prefs = Prefs(this)
            val log = logFile(this)
            log.writeText("")
            val prepared = Engine.prepare(prefs, log.path)
            NyxarApp.setupLibbox(this)
            prepared.xrayConfig?.let { startXray(it, prepared.xrayPort) }

            val server = CommandServer(this, this)
            server.start()
            commandServer = server
            server.startOrReloadService(prepared.config.toString(), OverrideOptions())

            VpnState.set(Status.ON, prepared.server)
            showNotification("Подключено: ${prepared.server.name}")
            startUsagePoll(prepared.apiPort, prefs)
        } catch (t: Throwable) {
            Log.e(TAG, "start failed", t)
            stopCore()
            VpnState.set(Status.ERROR, error = t.message ?: t.toString())
            stopForegroundCompat()
            stopSelf()
        }
    }

    private var poller: java.util.concurrent.ScheduledExecutorService? = null

    /** Раз в 2 секунды берёт у ядра список соединений и считает трафик по программам. */
    private fun startUsagePoll(port: Int, prefs: Prefs) {
        if (port == 0) return
        val tracker = UsageTracker(prefs)
        tracker.newSession()
        val ex = Executors.newSingleThreadScheduledExecutor()
        poller = ex
        ex.scheduleWithFixedDelay({
            try {
                val c = URL("http://127.0.0.1:$port/connections").openConnection() as HttpURLConnection
                c.connectTimeout = 1500
                c.readTimeout = 1500
                val body = c.inputStream.bufferedReader().use { it.readText() }
                c.disconnect()
                tracker.reload()
                val changed = tracker.feed(body)
                tracker.checkLimit(prefs.limitMb)?.let { level ->
                    notifyLimit(if (level >= 2) "Месячный лимит трафика исчерпан" else "Израсходовано больше 80% месячного лимита")
                }
                if (changed) tracker.save()
            } catch (_: Exception) {
            }
        }, 3, 2, java.util.concurrent.TimeUnit.SECONDS)
        usage = tracker
    }

    private var usage: UsageTracker? = null

    private fun notifyLimit(text: String) {
        val nm = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        val b = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            android.app.Notification.Builder(this, CHANNEL)
        } else {
            @Suppress("DEPRECATION")
            android.app.Notification.Builder(this)
        }
        nm.notify(2, b.setContentTitle("Nyxar").setContentText(text).setSmallIcon(R.drawable.ic_launcher).setAutoCancel(true).build())
    }

    private fun stopCore() {
        poller?.shutdownNow()
        poller = null
        usage?.save()
        usage = null
        val server = commandServer
        commandServer = null
        if (server != null) {
            runCatching { server.closeService() }
            runCatching { server.close() }
        }
        watcher?.stop()
        watcher = null
        runCatching { tun?.close() }
        tun = null
        stopXray()
    }

    /**
     * Запускает Xray из libxray.so (лежит в nativeLibraryDir, оттуда запуск разрешён).
     * Приложение исключено из VPN (addDisallowedApplication), поэтому трафик xray к серверу идёт мимо туннеля.
     */
    private fun startXray(cfg: org.json.JSONObject, port: Int) {
        val exe = File(applicationInfo.nativeLibraryDir, "libxray.so")
        if (!exe.exists()) throw NyxarException("В приложении нет Xray (libxray.so), нужен для серверов xhttp")
        val cfgFile = File(filesDir, "xray.json")
        cfgFile.writeText(cfg.toString(2))
        val log = xrayLogFile(this)
        log.writeText("")
        val p = ProcessBuilder(exe.path, "run", "-c", cfgFile.path)
            .directory(filesDir)
            .redirectErrorStream(true)
            .redirectOutput(log)
            .start()
        xray = p

        // ждём, пока xray начнёт слушать порт
        val deadline = System.currentTimeMillis() + 8000
        while (true) {
            if (!p.isAlive) {
                val tail = log.readText().lines().filter { it.isNotBlank() }.takeLast(8).joinToString("\n")
                throw NyxarException("xray остановился (${p.exitValue()}):\n$tail")
            }
            try {
                java.net.Socket().use { it.connect(java.net.InetSocketAddress("127.0.0.1", port), 200) }
                break
            } catch (_: java.io.IOException) {
            }
            if (System.currentTimeMillis() > deadline) {
                val tail = log.readText().lines().filter { it.isNotBlank() }.takeLast(8).joinToString("\n")
                throw NyxarException("xray не запустился за 8 секунд:\n$tail")
            }
            Thread.sleep(100)
        }
    }

    private fun stopXray() {
        val p = xray ?: return
        xray = null
        p.destroy()
        runCatching { p.waitFor(2, java.util.concurrent.TimeUnit.SECONDS) }
        if (p.isAlive) p.destroyForcibly()
    }

    // ───────────────────────── уведомление ─────────────────────────

    private fun showNotification(text: String) {
        val nm = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        val open = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val builder = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            nm.createNotificationChannel(
                NotificationChannel(CHANNEL, "Nyxar", NotificationManager.IMPORTANCE_LOW),
            )
            android.app.Notification.Builder(this, CHANNEL)
        } else {
            @Suppress("DEPRECATION")
            android.app.Notification.Builder(this)
        }
        val n = builder
            .setContentTitle("Nyxar")
            .setContentText(text)
            .setSmallIcon(R.drawable.ic_launcher)
            .setContentIntent(open)
            .setOngoing(true)
            .build()
        if (Build.VERSION.SDK_INT >= 34) {
            startForeground(NOTIFICATION_ID, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_SYSTEM_EXEMPTED)
        } else {
            startForeground(NOTIFICATION_ID, n)
        }
    }

    private fun stopForegroundCompat() {
        if (Build.VERSION.SDK_INT >= 24) stopForeground(STOP_FOREGROUND_REMOVE) else {
            @Suppress("DEPRECATION")
            stopForeground(true)
        }
    }

    // ───────────────────────── PlatformInterface ─────────────────────────

    override fun localDNSTransport(): LocalDNSTransport? = null

    override fun usePlatformAutoDetectInterfaceControl(): Boolean = true

    override fun autoDetectInterfaceControl(fd: Int) {
        protect(fd)
    }

    override fun openTun(options: TunOptions): Int {
        if (prepare(this) != null) error("Нет разрешения на VPN")

        val builder = Builder().setSession("Nyxar").setMtu(options.mtu)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) builder.setMetered(false)

        // само приложение (подписка, пинг) ходит мимо туннеля, ядро защищает свои сокеты через protect()
        try {
            builder.addDisallowedApplication(packageName)
        } catch (_: NameNotFoundException) {
        }
        // программы, которые пользователь пустил мимо VPN
        for (pkg in Prefs(this).bypassApps) {
            try {
                builder.addDisallowedApplication(pkg)
            } catch (_: NameNotFoundException) {
            }
        }

        val inet4 = options.inet4Address
        while (inet4.hasNext()) {
            val a = inet4.next()
            builder.addAddress(a.address(), a.prefix())
        }
        val inet6 = options.inet6Address
        while (inet6.hasNext()) {
            val a = inet6.next()
            builder.addAddress(a.address(), a.prefix())
        }

        if (options.autoRoute) {
            if (options.dnsMode.value != Libbox.DNSModeDisabled) {
                val dns = options.dnsServerAddress
                while (dns.hasNext()) builder.addDnsServer(dns.next())
            }

            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                val r4 = options.inet4RouteAddress
                if (r4.hasNext()) {
                    while (r4.hasNext()) builder.addRoute(r4.next().toIpPrefix())
                } else if (options.inet4Address.hasNext()) {
                    builder.addRoute("0.0.0.0", 0)
                }
                val r6 = options.inet6RouteAddress
                if (r6.hasNext()) {
                    while (r6.hasNext()) builder.addRoute(r6.next().toIpPrefix())
                } else if (options.inet6Address.hasNext()) {
                    builder.addRoute("::", 0)
                }
                val e4 = options.inet4RouteExcludeAddress
                while (e4.hasNext()) builder.excludeRoute(e4.next().toIpPrefix())
                val e6 = options.inet6RouteExcludeAddress
                while (e6.hasNext()) builder.excludeRoute(e6.next().toIpPrefix())
            } else {
                val r4 = options.inet4RouteRange
                while (r4.hasNext()) {
                    val a = r4.next()
                    builder.addRoute(a.address(), a.prefix())
                }
                val r6 = options.inet6RouteRange
                while (r6.hasNext()) {
                    val a = r6.next()
                    builder.addRoute(a.address(), a.prefix())
                }
            }
        }

        val pfd = builder.establish() ?: error("Не удалось создать VPN-интерфейс")
        tun = pfd
        return pfd.fd
    }

    override fun findConnectionOwner(
        ipProtocol: Int,
        sourceAddress: String?,
        sourcePort: Int,
        destinationAddress: String?,
        destinationPort: Int,
    ): ConnectionOwner {
        // нужно ядру, чтобы знать, какое приложение открыло соединение (учёт трафика по программам)
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) error("not supported")
        val cm = getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager
        val uid = cm.getConnectionOwnerUid(
            ipProtocol,
            InetSocketAddress(sourceAddress, sourcePort),
            InetSocketAddress(destinationAddress, destinationPort),
        )
        if (uid == Process.INVALID_UID) error("android: connection owner not found")
        val packages = packageManager.getPackagesForUid(uid)?.toList() ?: emptyList()
        val owner = ConnectionOwner()
        owner.userId = uid
        owner.userName = packages.firstOrNull() ?: ""
        owner.setAndroidPackageNames(StringArray(packages))
        return owner
    }

    override fun startDefaultInterfaceMonitor(listener: InterfaceUpdateListener) {
        val w = NetworkWatcher(this)
        watcher = w
        w.start(listener)
    }

    override fun closeDefaultInterfaceMonitor(listener: InterfaceUpdateListener) {
        watcher?.stop()
        watcher = null
    }

    override fun getInterfaces(): NetworkInterfaceIterator {
        val cm = getSystemService(Context.CONNECTIVITY_SERVICE) as android.net.ConnectivityManager
        val system = java.net.NetworkInterface.getNetworkInterfaces()?.toList().orEmpty()
        val result = mutableListOf<BoxInterface>()
        for (network in cm.allNetworks) {
            val props = cm.getLinkProperties(network) ?: continue
            val caps = cm.getNetworkCapabilities(network) ?: continue
            val name = props.interfaceName ?: continue
            val ni = system.find { it.name == name } ?: continue
            val box = BoxInterface()
            box.name = name
            box.index = ni.index
            runCatching { box.mtu = ni.mtu }
            box.dnsServer = StringArray(props.dnsServers.mapNotNull { it.hostAddress?.substringBefore('%') })
            box.gateway = StringArray(
                props.routes
                    .filter { it.destination.prefixLength == 0 }
                    .mapNotNull { it.gateway }
                    .filterNot { it.isAnyLocalAddress }
                    .mapNotNull { it.hostAddress?.substringBefore('%') },
            )
            box.addresses = StringArray(ni.interfaceAddresses.map { it.toPrefix() })
            box.type = when {
                caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) -> Libbox.InterfaceTypeWIFI
                caps.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR) -> Libbox.InterfaceTypeCellular
                caps.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET) -> Libbox.InterfaceTypeEthernet
                else -> Libbox.InterfaceTypeOther
            }
            var flags = 0
            if (caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)) {
                flags = OsConstants.IFF_UP or OsConstants.IFF_RUNNING
            }
            if (ni.isLoopback) flags = flags or OsConstants.IFF_LOOPBACK
            if (ni.isPointToPoint) flags = flags or OsConstants.IFF_POINTOPOINT
            if (ni.supportsMulticast()) flags = flags or OsConstants.IFF_MULTICAST
            box.flags = flags
            box.metered = !caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_METERED)
            result.add(box)
        }
        return InterfaceArray(result.iterator())
    }

    private fun InterfaceAddress.toPrefix(): String {
        val host = address.hostAddress?.substringBefore('%') ?: ""
        return "$host/$networkPrefixLength"
    }

    override fun underNetworkExtension(): Boolean = false

    override fun includeAllNetworks(): Boolean = false

    override fun readWIFIState(): WIFIState? = null

    override fun clearDNSCache() {}

    override fun sendNotification(notification: BoxNotification?) {}

    override fun cancelNotification(identifier: String?, typeID: Int) {}

    override fun startNeighborMonitor(listener: NeighborUpdateListener?) {}

    override fun closeNeighborMonitor(listener: NeighborUpdateListener?) {}

    override fun registerMyInterface(name: String?) {}

    override fun usePlatformShell(): Boolean = false

    override fun checkPlatformShell() {
        error("not supported")
    }

    override fun openShellSession(
        user: PlatformUser?,
        command: String?,
        environ: StringIterator?,
        term: String?,
        rows: Int,
        cols: Int,
    ): ShellSession = error("not supported")

    override fun lookupUser(username: String?): PlatformUser = error("not supported")

    override fun lookupSFTPServer(): String = error("not supported")

    override fun readSystemSSHHostKey(): String = error("not supported")

    override fun tailscaleHostname(): String = ""

    override fun usePlatformBridge(): Boolean = false

    override fun createBridge(options: BridgeOptions?): BridgeSession = error("not supported")

    // ───────────────────────── CommandServerHandler ─────────────────────────

    override fun serviceStop() {
        worker.execute {
            stopCore()
            VpnState.set(Status.OFF)
            stopForegroundCompat()
            stopSelf()
        }
    }

    override fun serviceReload() {}

    override fun getSystemProxyStatus(): SystemProxyStatus? = null

    override fun setSystemProxyEnabled(enabled: Boolean) {}

    override fun triggerNativeCrash() {}

    override fun writeDebugMessage(message: String?) {}

    override fun connectSSHAgent(): Int = error("not supported")
}

