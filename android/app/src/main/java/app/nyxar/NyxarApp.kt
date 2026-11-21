package app.nyxar

import android.app.Application
import android.content.Context
import android.os.Build
import io.nekohasekai.libbox.Libbox
import io.nekohasekai.libbox.SetupOptions

class NyxarApp : Application() {
    companion object {
        private var done = false

        /** Один раз настраивает libbox: рабочие папки и параметры ядра. */
        @Synchronized
        fun setupLibbox(ctx: Context) {
            if (done) return
            val opts = SetupOptions()
            opts.basePath = ctx.filesDir.path
            opts.workingPath = ctx.filesDir.path
            opts.tempPath = ctx.cacheDir.path
            // https://github.com/golang/go/issues/68760
            opts.fixAndroidStack = Build.VERSION.SDK_INT in 24..25 || Build.VERSION.SDK_INT >= 28
            opts.logMaxLines = 3000
            opts.debug = false
            opts.crashReportSource = "Nyxar"
            opts.appVersion = BuildConfig.VERSION_CODE.toString()
            opts.appMarketingVersion = BuildConfig.VERSION_NAME
            Libbox.setup(opts)
            done = true
        }
    }
}
