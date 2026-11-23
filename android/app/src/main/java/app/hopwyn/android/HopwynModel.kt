package app.hopwyn.android

import android.app.Application
import android.content.Context
import android.os.Build
import android.os.PowerManager
import android.provider.Settings
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Dispatchers
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

/**
 * A paired computer. [seen]: it is announcing itself on this network.
 * [address]: where it was last reached, or what the user typed.
 */
data class Computer(
    val id: String,
    val name: String,
    val online: Boolean,
    val seen: Boolean = false,
    val address: String = "",
    /** It drops every link at once: it has most likely forgotten this phone. */
    val dropping: Boolean = false,
)

data class NearbyComputer(val id: String, val name: String, val paired: Boolean, val open: Boolean)

sealed interface PairingState {
    data object None : PairingState
    /** "Pair a computer" is open, waiting for one to join. */
    data object Open : PairingState
    /** Connecting to the address the user typed. */
    data class Reaching(val address: String) : PairingState
    /** Both show [code]; the user checks and taps Pair. */
    data class Confirm(val name: String, val code: String) : PairingState
    /** This side tapped Pair; waiting for the computer. */
    data class Waiting(val name: String) : PairingState
    data class Failed(val why: String) : PairingState
}

/** Whether [text] looks like an address a computer shows: 192.168.1.20. */
fun looksLikeAddress(text: String): Boolean {
    val parts = text.trim().split('.')
    return parts.size == 4 && parts.all { p -> p.length in 1..3 && p.all(Char::isDigit) && p.toInt() <= 255 }
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
    /** How many pairings finished, so the pairing screen knows to close. */
    var pairedCount by mutableStateOf(0)
        private set
    var startError by mutableStateOf<String?>(null)
        private set
    /** Whether computers can drive this phone (the Accessibility service is on). */
    val controllable get() = ControlService.active
    /** Whether battery optimisation is off for Hopwyn, so it isn't stopped. */
    var background by mutableStateOf(runsInBackground(app))
        private set

    private val onEvent: (String) -> Unit = ::handle

    init {
        viewModelScope.launch {
            val error = withContext(Dispatchers.IO) { Core.start(app) }
            if (error != null) {
                startError = error
                return@launch
            }
            Core.attach(onEvent)
        }
    }

    override fun onCleared() = Core.detach(onEvent)

    /** Re-reads what the user may have changed in Android's settings. */
    fun refresh() {
        background = runsInBackground(getApplication())
    }

    private fun handle(line: String) {
        val f = line.split('\u001f')
        val records = { s: String? ->
            s.orEmpty().split('\u001e').filter { it.isNotEmpty() }.map { it.split('\u001d') }
        }
        when (f.firstOrNull()) {
            "ready" -> deviceName = f.getOrElse(1) { "" }
            "partners" -> computers = records(f.getOrNull(1)).mapNotNull {
                if (it.size < 3) {
                    null
                } else {
                    Computer(
                        it[0],
                        it[1],
                        it[2] == "1",
                        it.getOrNull(3) == "1",
                        it.getOrElse(4) { "" },
                        it.getOrNull(5) == "1",
                    )
                }
            }
            "nearby" -> nearby = records(f.getOrNull(1)).mapNotNull {
                if (it.size < 4) null else NearbyComputer(it[0], it[1], it[2] == "1", it[3] == "1")
            }
            "pairing-open" -> pairing = PairingState.Open
            "pairing-confirm" -> pairing = PairingState.Confirm(f.getOrElse(1) { "" }, f.getOrElse(2) { "" })
            "pairing-done" -> {
                pairedCount++
                pairing = PairingState.None
                message = "Paired with ${f.getOrElse(1) { "" }}"
            }
            "pairing-failed" -> {
                val why = f.getOrElse(1) { "" }
                // Nobody joined before the window closed: not worth a word.
                val quiet = pairing is PairingState.Open && "answer in time" in why
                pairing = if (quiet) PairingState.None else PairingState.Failed(explain(why))
            }
            "pairing-closed" -> pairing = PairingState.None
            "controlling" -> controlling = f.getOrNull(1)?.takeIf { it.isNotEmpty() }
            "error" -> message = f.getOrElse(1) { "" }.replaceFirstChar(Char::uppercase)
        }
    }

    /** Says what went wrong in terms of what to do about it. */
    private fun explain(why: String): String {
        val w = why.lowercase()
        return when {
            "refused" in w ->
                "That computer isn't waiting to pair. Click Add computer in Hopwyn on it, then try again."
            "unreachable" in w || "no route" in w || "answer in time" in w || "timed out" in w ->
                "Couldn't reach the computer. Check that it and this phone are on the same network, and the address is right."
            "cancelled on the other" in w -> "Pairing was cancelled on the computer."
            "tampered" in w -> "The codes didn't match, so nothing was paired. Try again."
            else -> why.replaceFirstChar(Char::uppercase)
        }
    }

    /** Opens the pairing window; nearby computers can join it. */
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

    fun joinAddress(address: String) {
        pairing = PairingState.Reaching(address.trim())
        Native.nativeJoinAddress(address.trim())
    }

    fun setAddress(id: String, address: String) = Native.nativeSetAddress(id, address.trim())
    fun control(id: String) = Native.nativeControl(id)
    fun release() = Native.nativeRelease()
    fun forget(id: String) = Native.nativeForget(id)
}
