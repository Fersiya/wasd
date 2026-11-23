package app.nyxar

import android.content.Intent
import android.os.Build
import android.service.quicksettings.Tile
import android.service.quicksettings.TileService

class NyxarTileService : TileService() {
    private val listener = { updateTile() }

    override fun onStartListening() {
        super.onStartListening()
        VpnState.listen(listener)
        updateTile()
    }

    override fun onStopListening() {
        super.onStopListening()
        VpnState.unlisten(listener)
    }

    override fun onClick() {
        super.onClick()
        val on = VpnState.status == Status.ON || VpnState.status == Status.CONNECTING
        val intent = Intent(this, NyxarVpnService::class.java).apply {
            action = if (on) NyxarVpnService.ACTION_STOP else NyxarVpnService.ACTION_START
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            startForegroundService(intent)
        } else {
            startService(intent)
        }
    }

    private fun updateTile() {
        val tile = qsTile ?: return
        when (VpnState.status) {
            Status.ON -> {
                tile.state = Tile.STATE_ACTIVE
                tile.label = "Nyxar: В сети"
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                    tile.subtitle = VpnState.serverName.ifEmpty { "Подключено" }
                }
            }
            Status.CONNECTING -> {
                tile.state = Tile.STATE_UNAVAILABLE
                tile.label = "Nyxar: Запуск"
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                    tile.subtitle = "Подключение…"
                }
            }
            Status.ERROR -> {
                tile.state = Tile.STATE_INACTIVE
                tile.label = "Nyxar: Сбой"
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                    tile.subtitle = "Нажмите для попытки"
                }
            }
            Status.OFF -> {
                tile.state = Tile.STATE_INACTIVE
                tile.label = "Nyxar VPN"
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                    tile.subtitle = "Отключено"
                }
            }
        }
        tile.updateTile()
    }
}
