package app.hopwyn.android

import android.accessibilityservice.AccessibilityService
import android.accessibilityservice.GestureDescription
import android.content.res.Configuration
import android.graphics.Path
import android.graphics.PixelFormat
import android.net.wifi.WifiManager
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.util.DisplayMetrics
import android.view.Gravity
import android.view.WindowManager
import android.view.accessibility.AccessibilityEvent
import android.view.accessibility.AccessibilityNodeInfo
import android.widget.ImageView
import kotlin.concurrent.thread

/**
 * Lets a paired computer's mouse and keyboard drive this phone. The Rust
 * core tracks the pointer and decides what each click, drag, scroll and
 * key means; this service only shows the pointer and performs the result:
 * gestures, Back and Home, and text in the focused field.
 *
 * It also keeps Hopwyn running (and linked) while the app is closed.
 */
class ControlService : AccessibilityService() {
    companion object {
        /** Whether the service is on, for the app's status card. */
        @Volatile
        var active = false
            private set
    }

    @Volatile
    private var running = false
    private val main = Handler(Looper.getMainLooper())
    private lateinit var windows: WindowManager
    private var pointer: ImageView? = null
    private var pointerParams: WindowManager.LayoutParams? = null
    // Finding computers (mDNS) needs multicast, which Android drops unless
    // asked; the app's screen may not be open while this service runs.
    private var multicast: WifiManager.MulticastLock? = null

    override fun onServiceConnected() {
        windows = getSystemService(WINDOW_SERVICE) as WindowManager
        multicast = (applicationContext.getSystemService(WIFI_SERVICE) as WifiManager)
            .createMulticastLock("hopwyn-control")
            .apply {
                setReferenceCounted(false)
                acquire()
            }
        Native.nativeStart(filesDir.absolutePath, deviceName(this))
        reportSize()
        active = true
        running = true
        thread(name = "hopwyn-control", isDaemon = true) {
            while (running) {
                val line = Native.nativeNextControl(250) ?: continue
                main.post { perform(line) }
            }
        }
    }

    override fun onConfigurationChanged(newConfig: Configuration) {
        super.onConfigurationChanged(newConfig)
        if (running) reportSize()
    }

    override fun onUnbind(intent: android.content.Intent?): Boolean {
        stop()
        return super.onUnbind(intent)
    }

    override fun onDestroy() {
        stop()
        super.onDestroy()
    }

    override fun onAccessibilityEvent(event: AccessibilityEvent?) {}

    override fun onInterrupt() {}

    private fun stop() {
        if (!running) return
        running = false
        active = false
        Native.nativeSetControllable(false, 0, 0)
        hidePointer()
        multicast?.release()
        multicast = null
    }

    /** Tells the core the full screen size (rotation changes it). */
    private fun reportSize() {
        val (w, h) = if (Build.VERSION.SDK_INT >= 30) {
            val b = windows.currentWindowMetrics.bounds
            b.width() to b.height()
        } else {
            val m = DisplayMetrics()
            @Suppress("DEPRECATION")
            windows.defaultDisplay.getRealMetrics(m)
            m.widthPixels to m.heightPixels
        }
        Native.nativeSetControllable(true, w, h)
    }

    private fun perform(line: String) {
        val f = line.split('\u001f')
        val num = { i: Int -> f.getOrNull(i)?.toFloatOrNull() ?: 0f }
        when (f.firstOrNull()) {
            "show" -> showPointer(num(1), num(2))
            "move" -> movePointer(num(1), num(2))
            "hide" -> hidePointer()
            "tap" -> gesture(Path().apply { moveTo(num(1), num(2)) }, 40)
            "swipe" -> gesture(
                Path().apply {
                    moveTo(num(1), num(2))
                    lineTo(num(3), num(4))
                },
                (f.getOrNull(5)?.toLongOrNull() ?: 300L).coerceIn(10L, 2000L),
            )
            "back" -> performGlobalAction(GLOBAL_ACTION_BACK)
            "home" -> performGlobalAction(GLOBAL_ACTION_HOME)
            "text" -> edit { text, start, end ->
                val typed = f.getOrElse(1) { "" }
                Pair(text.substring(0, start) + typed + text.substring(end), start + typed.length)
            }
            "backspace" -> edit { text, start, end ->
                if (start != end) {
                    Pair(text.substring(0, start) + text.substring(end), start)
                } else if (start > 0) {
                    Pair(text.substring(0, start - 1) + text.substring(end), start - 1)
                } else {
                    null
                }
            }
            "enter" -> enter()
        }
    }

    private fun gesture(path: Path, ms: Long) {
        val stroke = GestureDescription.StrokeDescription(path, 0, ms)
        dispatchGesture(GestureDescription.Builder().addStroke(stroke).build(), null, null)
    }

    private fun showPointer(x: Float, y: Float) {
        if (pointer == null) {
            val view = ImageView(this).apply { setImageResource(R.drawable.pointer) }
            val params = WindowManager.LayoutParams(
                WindowManager.LayoutParams.WRAP_CONTENT,
                WindowManager.LayoutParams.WRAP_CONTENT,
                WindowManager.LayoutParams.TYPE_ACCESSIBILITY_OVERLAY,
                WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE or
                    WindowManager.LayoutParams.FLAG_NOT_TOUCHABLE or
                    WindowManager.LayoutParams.FLAG_LAYOUT_NO_LIMITS or
                    WindowManager.LayoutParams.FLAG_LAYOUT_IN_SCREEN,
                PixelFormat.TRANSLUCENT,
            ).apply {
                gravity = Gravity.TOP or Gravity.START
                if (Build.VERSION.SDK_INT >= 28) {
                    layoutInDisplayCutoutMode =
                        WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES
                }
            }
            runCatching { windows.addView(view, params) }.onSuccess {
                pointer = view
                pointerParams = params
            }
        }
        movePointer(x, y)
    }

    private fun movePointer(x: Float, y: Float) {
        val view = pointer ?: return showPointer(x, y)
        val params = pointerParams ?: return
        params.x = x.toInt()
        params.y = y.toInt()
        runCatching { windows.updateViewLayout(view, params) }
    }

    private fun hidePointer() {
        pointer?.let { runCatching { windows.removeView(it) } }
        pointer = null
        pointerParams = null
    }

    private fun focusedField(): AccessibilityNodeInfo? =
        rootInActiveWindow?.findFocus(AccessibilityNodeInfo.FOCUS_INPUT)?.takeIf { it.isEditable }

    /**
     * Edits the focused text field: [change] gets its text and selection
     * and returns the new text and cursor position (or null for no change).
     */
    private fun edit(change: (String, Int, Int) -> Pair<String, Int>?) {
        val node = focusedField() ?: return
        val hint = Build.VERSION.SDK_INT >= 26 && node.isShowingHintText
        val text = if (hint) "" else node.text?.toString().orEmpty()
        val start = node.textSelectionStart.takeIf { it in 0..text.length } ?: text.length
        val end = node.textSelectionEnd.takeIf { it in start..text.length } ?: start
        val (newText, cursor) = change(text, start, end) ?: return
        node.performAction(
            AccessibilityNodeInfo.ACTION_SET_TEXT,
            Bundle().apply {
                putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE, newText)
            },
        )
        node.performAction(
            AccessibilityNodeInfo.ACTION_SET_SELECTION,
            Bundle().apply {
                putInt(AccessibilityNodeInfo.ACTION_ARGUMENT_SELECTION_START_INT, cursor)
                putInt(AccessibilityNodeInfo.ACTION_ARGUMENT_SELECTION_END_INT, cursor)
            },
        )
    }

    private fun enter() {
        val node = focusedField() ?: return
        if (Build.VERSION.SDK_INT >= 30) {
            node.performAction(AccessibilityNodeInfo.AccessibilityAction.ACTION_IME_ENTER.id)
        } else {
            edit { text, start, end -> Pair(text.substring(0, start) + "\n" + text.substring(end), start + 1) }
        }
    }
}
