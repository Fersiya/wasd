package app.nyxar

import android.animation.ValueAnimator
import android.content.Context
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.LinearGradient
import android.graphics.Paint
import android.graphics.RectF
import android.graphics.Shader
import android.view.MotionEvent
import android.view.View
import android.view.animation.LinearInterpolator

/** Круглая кнопка питания: градиент, свечение, расходящиеся кольца при подключении. */
class PowerButton(context: Context) : View(context) {
    private val d = resources.displayMetrics.density
    private val fill = Paint(Paint.ANTI_ALIAS_FLAG)
    private val ring = Paint(Paint.ANTI_ALIAS_FLAG).apply { style = Paint.Style.STROKE; strokeWidth = 2.5f * d }
    private val glow = Paint(Paint.ANTI_ALIAS_FLAG)
    private val icon = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.STROKE
        strokeCap = Paint.Cap.ROUND
        color = Color.WHITE
    }
    private val box = RectF()
    private var c1 = Color.parseColor("#8B7BFF")
    private var c2 = Color.parseColor("#5B4BE6")
    private var phase = 0f
    private var anim: ValueAnimator? = null
    private var fast = false

    fun setStatus(s: Status) {
        when (s) {
            Status.OFF -> { c1 = Color.parseColor("#8B7BFF"); c2 = Color.parseColor("#5B4BE6") }
            Status.CONNECTING -> { c1 = Color.parseColor("#FFB454"); c2 = Color.parseColor("#FF8A3D") }
            Status.ON -> { c1 = Color.parseColor("#3EE08F"); c2 = Color.parseColor("#1FA866") }
            Status.ERROR -> { c1 = Color.parseColor("#FF7B7B"); c2 = Color.parseColor("#E04848") }
        }
        fast = s == Status.CONNECTING
        val animated = s == Status.CONNECTING || s == Status.ON
        if (animated) {
            if (anim == null) {
                anim = ValueAnimator.ofFloat(0f, 1f).apply {
                    repeatCount = ValueAnimator.INFINITE
                    interpolator = LinearInterpolator()
                    addUpdateListener { phase = it.animatedValue as Float; invalidate() }
                    start()
                }
            }
            anim?.duration = if (fast) 1300 else 3200
        } else {
            anim?.cancel()
            anim = null
            phase = 0f
        }
        invalidate()
    }

    override fun onDraw(canvas: Canvas) {
        val cx = width / 2f
        val cy = height / 2f
        val max = minOf(width, height) / 2f
        val base = max * 0.62f

        // мягкое свечение
        glow.color = c1
        glow.alpha = 38
        canvas.drawCircle(cx, cy, base * 1.2f, glow)
        glow.alpha = 20
        canvas.drawCircle(cx, cy, base * 1.38f, glow)

        // расходящиеся кольца
        if (anim != null) {
            for (k in 0..1) {
                val p = (phase + k * 0.5f) % 1f
                ring.color = c1
                ring.alpha = ((1f - p) * 150).toInt()
                canvas.drawCircle(cx, cy, base + (max - base) * p, ring)
            }
        }

        // сама кнопка
        fill.shader = LinearGradient(cx, cy - base, cx, cy + base, c1, c2, Shader.TileMode.CLAMP)
        canvas.drawCircle(cx, cy, base, fill)
        ring.color = Color.WHITE
        ring.alpha = 45
        canvas.drawCircle(cx, cy, base - 1.5f * d, ring)

        // значок питания
        val r = base * 0.38f
        icon.strokeWidth = base * 0.11f
        box.set(cx - r, cy - r, cx + r, cy + r)
        canvas.drawArc(box, -60f, 300f, false, icon)
        canvas.drawLine(cx, cy - r * 1.12f, cx, cy - r * 0.1f, icon)
    }

    override fun onTouchEvent(e: MotionEvent): Boolean {
        when (e.actionMasked) {
            MotionEvent.ACTION_DOWN -> animate().scaleX(0.94f).scaleY(0.94f).setDuration(90).start()
            MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> animate().scaleX(1f).scaleY(1f).setDuration(160).start()
        }
        return super.onTouchEvent(e)
    }

    override fun onDetachedFromWindow() {
        anim?.cancel()
        anim = null
        super.onDetachedFromWindow()
    }
}
