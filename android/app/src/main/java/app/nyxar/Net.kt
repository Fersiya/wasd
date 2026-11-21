package app.nyxar

import android.content.Context
import android.net.ConnectivityManager
import android.net.LinkProperties
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import io.nekohasekai.libbox.InterfaceUpdateListener
import io.nekohasekai.libbox.NetworkInterfaceIterator
import io.nekohasekai.libbox.RoutePrefix
import io.nekohasekai.libbox.StringIterator
import java.net.InetAddress
import java.net.NetworkInterface
import io.nekohasekai.libbox.NetworkInterface as BoxInterface

/** Итератор строк для libbox. */
class StringArray(private val items: List<String>) : StringIterator {
    private var pos = 0
    override fun len(): Int = items.size - pos
    override fun hasNext(): Boolean = pos < items.size
    override fun next(): String = items[pos++]
}

class InterfaceArray(private val iterator: Iterator<BoxInterface>) : NetworkInterfaceIterator {
    override fun hasNext(): Boolean = iterator.hasNext()
    override fun next(): BoxInterface = iterator.next()
}

fun RoutePrefix.toIpPrefix() = android.net.IpPrefix(InetAddress.getByName(address()), prefix())

/**
 * Следит за сетью, по которой реально ходит интернет (без самого VPN), и сообщает
 * ядру её интерфейс. registerDefaultNetworkCallback после запуска VPN вернул бы сам туннель.
 */
class NetworkWatcher(context: Context) {
    private val cm = context.getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager
    private val handler by lazy { Handler(HandlerThread("nyxar-net").apply { start() }.looper) }

    @Volatile private var listener: InterfaceUpdateListener? = null
    @Volatile private var network: Network? = null
    private var registered = false

    private val request = NetworkRequest.Builder()
        .addCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
        .addCapability(NetworkCapabilities.NET_CAPABILITY_NOT_RESTRICTED)
        .build()

    private val callback = object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) {
            this@NetworkWatcher.network = network
            publish()
        }

        override fun onLinkPropertiesChanged(network: Network, linkProperties: LinkProperties) {
            if (this@NetworkWatcher.network == network) publish()
        }

        override fun onLost(network: Network) {
            if (this@NetworkWatcher.network == network) {
                this@NetworkWatcher.network = null
                publish()
            }
        }
    }

    fun start(l: InterfaceUpdateListener) {
        listener = l
        network = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) cm.activeNetwork else null
        if (!registered) {
            registered = true
            when {
                Build.VERSION.SDK_INT >= 31 -> cm.registerBestMatchingNetworkCallback(request, callback, handler)
                Build.VERSION.SDK_INT >= 28 -> cm.requestNetwork(request, callback, handler)
                else -> cm.registerDefaultNetworkCallback(callback, handler)
            }
        }
        publish()
    }

    fun stop() {
        listener = null
        if (registered) {
            registered = false
            runCatching { cm.unregisterNetworkCallback(callback) }
        }
    }

    private fun publish() {
        val l = listener ?: return
        val n = network
        if (n == null) {
            l.updateDefaultInterface("", -1, false, false)
            return
        }
        // интерфейс появляется чуть позже самой сети, поэтому несколько попыток
        for (attempt in 0 until 10) {
            val name = cm.getLinkProperties(n)?.interfaceName
            val index = name?.let { runCatching { NetworkInterface.getByName(it)?.index }.getOrNull() }
            if (name != null && index != null) {
                l.updateDefaultInterface(name, index, false, false)
                return
            }
            Thread.sleep(100)
        }
    }
}
