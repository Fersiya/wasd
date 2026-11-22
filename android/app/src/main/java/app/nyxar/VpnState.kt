package app.nyxar

import android.os.Handler
import android.os.Looper
import java.util.concurrent.CopyOnWriteArrayList

enum class Status { OFF, CONNECTING, ON, ERROR }

/** Общее состояние туннеля: пишет сервис, читает окно. */
object VpnState {
    @Volatile var status = Status.OFF
        private set
    @Volatile var serverName = ""
        private set
    @Volatile var serverKey = ""
        private set
    @Volatile var error = ""
        private set
    /** Момент (elapsedRealtime), с которого туннель поднят. */
    @Volatile var since = 0L
        private set

    private val listeners = CopyOnWriteArrayList<() -> Unit>()
    private val main = Handler(Looper.getMainLooper())

    fun listen(l: () -> Unit) {
        listeners.add(l)
    }

    fun unlisten(l: () -> Unit) {
        listeners.remove(l)
    }

    fun set(status: Status, server: Server? = null, error: String = "") {
        if (status == Status.ON && this.status != Status.ON) since = android.os.SystemClock.elapsedRealtime()
        this.status = status
        this.serverName = server?.name ?: ""
        this.serverKey = server?.key ?: ""
        this.error = error
        main.post { listeners.forEach { it() } }
    }
}
