package app.hopwyn.android

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.net.ConnectivityManager
import android.net.LinkProperties
import android.net.Network
import android.net.NetworkCapabilities
import android.net.wifi.WifiManager
import android.os.Handler
import android.os.Looper
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import kotlin.concurrent.thread

/** What the phone is connected to, as far as reaching computers goes. */
enum class NetworkKind {
    /** Wi-Fi or Ethernet: computers on it can be reached. */
    Local,
    /** Mobile data only. Computers can still join this phone's hotspot. */
    Mobile,
    None,
}

/**
 * The one Rust core in this process, shared by the app's screens and the
 * Accessibility service, with what both need around it: the phone's
 * network, and permission to hear other devices announce themselves.
 */
object Core {
    /** How long to keep listening for announcements with no screen open. */
    private const val LISTEN_MS = 120_000L

    var network by mutableStateOf(NetworkKind.None)
        private set

    private val main = Handler(Looper.getMainLooper())
    private var started = false
    private var startError: String? = null
    private var multicast: WifiManager.MulticastLock? = null
    private var foreground = false
    private var listening = false
    private var lastNetwork: String? = null
    private val changed = Runnable {
        Native.nativeNetworkChanged()
        listenForAWhile()
    }
    private val stopListening = Runnable {
        listening = false
        updateLock()
    }

    /** Starts the core once. Returns an error message, or null. */
    @Synchronized
    fun start(context: Context): String? {
        if (started) return startError
        val app = context.applicationContext
        startError = Native.nativeStart(app.filesDir.absolutePath, deviceName(app))
        started = true
        if (startError == null) {
            appContext = app
            Native.nativeSetClipboard(Prefs.get(app).shareClipboard)
            main.post { watchNetwork(app) }
            readEvents()
        }
        return startError
    }

    // The newest line of each kind of state, for a screen opened later.
    private val latest = LinkedHashMap<String, String>()
    private var listener: ((String) -> Unit)? = null

    /**
     * Sends the core's event lines to [onEvent] on the main thread,
     * starting with the current state. One listener at a time.
     */
    fun attach(onEvent: (String) -> Unit) {
        listener = onEvent
        latest.values.toList().forEach(onEvent)
    }

    fun detach(onEvent: (String) -> Unit) {
        if (listener === onEvent) listener = null
    }

    /**
     * One thread for the life of the process takes the core's events as
     * they come, asleep in between, whether or not a screen is open: they
     * would pile up otherwise while only phone control is running.
     */
    private fun readEvents() {
        thread(name = "hopwyn-events", isDaemon = true) {
            while (true) {
                val line = Native.nativeNextEvent(60_000) ?: continue
                main.post { deliver(line) }
            }
        }
    }

    private var appContext: Context? = null
    // The newest copied text that went either way, and when the phone's
    // clipboard was last read, so nothing is sent twice or sent back.
    private var lastClip = ""
    private var lastClipTime = 0L

    /** Puts text copied on a computer onto the phone's clipboard. */
    private fun paste(text: String) {
        val app = appContext ?: return
        if (text.isEmpty() || text == lastClip || !Prefs.get(app).shareClipboard) return
        lastClip = text
        val clipboard = app.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
        runCatching { clipboard.setPrimaryClip(ClipData.newPlainText("Hopwyn", text)) }
        lastClipTime = runCatching { clipboard.primaryClipDescription?.timestamp }.getOrNull() ?: lastClipTime
    }

    /**
     * Sends what was copied on the phone to paired computers. Android only
     * lets the app on screen read the clipboard, so this runs when one of
     * Hopwyn's screens gets focus. The clipboard is read only when it holds
     * something new (Android tells the user each time an app reads it).
     */
    fun sendCopied(context: Context) {
        if (!started || startError != null || !Prefs.get(context).shareClipboard) return
        val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
        val about = runCatching { clipboard.primaryClipDescription }.getOrNull() ?: return
        if (about.timestamp == lastClipTime || about.label == "Hopwyn") return
        if (!about.hasMimeType("text/*")) return
        lastClipTime = about.timestamp
        val text = runCatching { clipboard.primaryClip?.getItemAt(0)?.text?.toString() }.getOrNull()
        if (text.isNullOrEmpty() || text == lastClip) return
        lastClip = text
        Native.nativeCopied(text)
    }

    private fun deliver(line: String) {
        when (val kind = line.substringBefore('\u001f')) {
            "clipboard" -> return paste(line.substringAfter('\u001f', ""))
            "ready", "partners", "nearby", "controlling" -> latest[kind] = line
            "pairing-open", "pairing-confirm" -> latest["pairing"] = line
            "pairing-done", "pairing-failed", "pairing-closed" -> latest.remove("pairing")
        }
        listener?.invoke(line)
    }

    /** Whether one of Hopwyn's screens is showing. */
    fun setForeground(on: Boolean) {
        foreground = on
        updateLock()
    }

    /**
     * Listens for other devices for a couple of minutes, for when no screen
     * is open: after the phone changes network, or phone control starts.
     * After that, computers are reached at the addresses that worked before.
     */
    fun listenForAWhile() {
        listening = true
        main.removeCallbacks(stopListening)
        main.postDelayed(stopListening, LISTEN_MS)
        updateLock()
    }

    // Android drops multicast packets (how devices announce themselves)
    // unless an app holds this lock, and holding it costs battery.
    private fun updateLock() {
        val lock = multicast ?: return
        val want = foreground || listening
        if (want && !lock.isHeld) lock.acquire()
        if (!want && lock.isHeld) lock.release()
    }

    private fun watchNetwork(app: Context) {
        multicast = (app.getSystemService(Context.WIFI_SERVICE) as WifiManager)
            .createMulticastLock("hopwyn")
            .apply { setReferenceCounted(false) }
        updateLock()
        val manager = app.getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager
        val callback = object : ConnectivityManager.NetworkCallback() {
            private var kind = NetworkKind.None
            private var addresses = ""

            override fun onCapabilitiesChanged(net: Network, caps: NetworkCapabilities) {
                kind = when {
                    caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) ||
                        caps.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET) -> NetworkKind.Local
                    else -> NetworkKind.Mobile
                }
                report(net)
            }

            override fun onLinkPropertiesChanged(net: Network, link: LinkProperties) {
                addresses = link.linkAddresses.map { it.address.hostAddress }.sorted().joinToString(",")
                report(net)
            }

            override fun onLost(net: Network) {
                kind = NetworkKind.None
                addresses = ""
                network = NetworkKind.None
                moved("none")
            }

            private fun report(net: Network) {
                network = kind
                moved("$net $kind $addresses")
            }
        }
        runCatching { manager.registerDefaultNetworkCallback(callback, main) }
    }

    /**
     * Tells the core when the network really is a different one. Android
     * reports several changes while joining a network, so this waits for
     * them to settle.
     */
    private fun moved(now: String) {
        val before = lastNetwork
        lastNetwork = now
        if (before == null || before == now) return
        main.removeCallbacks(changed)
        main.postDelayed(changed, 600)
    }
}
