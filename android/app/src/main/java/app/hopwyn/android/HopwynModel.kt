package app.hopwyn.android

import android.app.Application
import android.content.Context
import android.net.wifi.WifiManager
import android.os.Build
import android.os.PowerManager
import android.provider.Settings
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** Whether Android leaves Hopwyn running in the background. */
fun runsInBackground(context: Context): Boolean =
    (context.getSystemService(Context.POWER_SERVICE) as PowerManager)
        .isIgnoringBatteryOptimizations(context.packageName)

/** This phone's name as computers see it. */
fun deviceName(context: Context): String =
    Settings.Global.getString(context.contentResolver, Settings.Global.DEVICE_NAME)
        ?.takeIf { it.isNotBlank() }
        ?: Build.MODEL

data class Computer(val id: String, val name: String, val online: Boolean)

data class NearbyComputer(val id: String, val name: String, val paired: Boolean, val open: Boolean)

sealed interface PairingState {
    data object None : PairingState
    /** "Pair a computer" is open, waiting for one to join. */
    data object Open : PairingState
    /** Both show [code]; the user checks and clicks Pair. */
    data class Confirm(val name: String, val code: String) : PairingState
    /** This side clicked Pair; waiting for the computer. */
    data class Waiting(val name: String) : PairingState
}

/** App state, fed by the Rust core's event lines. */
class HopwynModel(app: Application) : AndroidViewModel(app) {
    var deviceName by mutableStateOf("")
        private set
    var computers by mutableStateOf(listOf<Computer>())
        private set
    var nearby by mutableStateOf(listOf<NearbyComputer>())
        private set
    var pairing by mutableStateOf<PairingState>(PairingState.None)
        private set
    /** The computer being driven, or null. */
    var controlling by mutableStateOf<String?>(null)
        private set
    var message by mutableStateOf<String?>(null)
    var startError by mutableStateOf<String?>(null)
        private set
    /** Whether computers can drive this phone (the Accessibility service is on). */
    var controllable by mutableStateOf(ControlService.active)
        private set
    /** Whether battery optimisation is off for Hopwyn, so it isn't stopped. */
    var background by mutableStateOf(runsInBackground(app))
        private set

    // mDNS needs multicast packets, which Android drops unless asked.
    private val multicast = (app.getSystemService(Context.WIFI_SERVICE) as WifiManager)
        .createMulticastLock("hopwyn")
        .apply { setReferenceCounted(false) }

    init {
        multicast.acquire()
        val name = deviceName(app)
        viewModelScope.launch {
            val error = withContext(Dispatchers.IO) {
                Native.nativeStart(app.filesDir.absolutePath, name)
            }
            if (error != null) {
                startError = error
                return@launch
            }
            while (isActive) {
                val line = withContext(Dispatchers.IO) { Native.nativeNextEvent(250) }
                controllable = ControlService.active
                background = runsInBackground(getApplication())
                if (line != null) handle(line)
            }
        }
    }

    override fun onCleared() {
        multicast.release()
    }

    private fun handle(line: String) {
        val f = line.split('\u001f')
        val records = { s: String? ->
            s.orEmpty().split('\u001e').filter { it.isNotEmpty() }.map { it.split('\u001d') }
        }
        when (f.firstOrNull()) {
            "ready" -> deviceName = f.getOrElse(1) { "" }
            "partners" -> computers = records(f.getOrNull(1)).mapNotNull {
                if (it.size < 3) null else Computer(it[0], it[1], it[2] == "1")
            }
            "nearby" -> nearby = records(f.getOrNull(1)).mapNotNull {
                if (it.size < 4) null else NearbyComputer(it[0], it[1], it[2] == "1", it[3] == "1")
            }
            "pairing-open" -> pairing = PairingState.Open
            "pairing-confirm" -> pairing = PairingState.Confirm(f.getOrElse(1) { "" }, f.getOrElse(2) { "" })
            "pairing-done" -> {
                pairing = PairingState.None
                message = "Paired with ${f.getOrElse(1) { "" }}"
            }
            "pairing-failed" -> {
                pairing = PairingState.None
                message = "Pairing failed: ${f.getOrElse(1) { "" }}"
            }
            "pairing-closed" -> pairing = PairingState.None
            "controlling" -> controlling = f.getOrNull(1)?.takeIf { it.isNotEmpty() }
            "error" -> message = f.getOrElse(1) { "" }
        }
    }

    fun addComputer() = Native.nativeAddComputer()

    fun cancelPairing() {
        Native.nativeCancelPairing()
        pairing = PairingState.None
    }

    fun confirm(yes: Boolean) {
        val p = pairing
        Native.nativeConfirm(yes)
        pairing = if (yes && p is PairingState.Confirm) PairingState.Waiting(p.name) else PairingState.None
    }

    fun join(id: String) = Native.nativeJoin(id)
    fun control(id: String) = Native.nativeControl(id)
    fun release() = Native.nativeRelease()
    fun forget(id: String) = Native.nativeForget(id)
}
