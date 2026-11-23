package app.hopwyn.android

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.SystemBarStyle
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.viewModels
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.BatteryAlert
import androidx.compose.material.icons.filled.Laptop
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.Mouse
import androidx.compose.material.icons.filled.Settings as SettingsIcon
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp

private enum class Screen { Home, Settings, Setup, Pair }

class MainActivity : ComponentActivity() {
    private val model: HopwynModel by viewModels()

    override fun onStart() {
        super.onStart()
        Core.setForeground(true)
    }

    override fun onResume() {
        super.onResume()
        // The user may be back from changing a setting.
        model.refresh()
    }

    override fun onStop() {
        Core.setForeground(false)
        super.onStop()
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val prefs = Prefs.get(this)
        setContent {
            HopwynTheme(prefs) {
                val controlling = model.controlling
                var screen by rememberSaveable { mutableStateOf(Screen.Home) }
                val home = { screen = Screen.Home }
                // A computer can start pairing whatever screen is showing.
                val comparing = model.pairing is PairingState.Confirm || model.pairing is PairingState.Waiting

                // System bar icons follow the screen shown, not the phone's theme.
                val trackpad = controlling != null && !comparing && screen != Screen.Pair
                val dark = trackpad || MaterialTheme.colorScheme.background.luminance() < 0.5f
                LaunchedEffect(dark) {
                    val clear = android.graphics.Color.TRANSPARENT
                    val style = if (dark) SystemBarStyle.dark(clear) else SystemBarStyle.light(clear, clear)
                    enableEdgeToEdge(statusBarStyle = style, navigationBarStyle = style)
                }

                when {
                    screen == Screen.Pair || comparing ->
                        PairScreen(model, byUser = screen == Screen.Pair, onClose = home)
                    controlling != null -> {
                        val name = model.computers.firstOrNull { it.id == controlling }?.name ?: "computer"
                        Trackpad(name = name, prefs = prefs, onDone = model::release)
                    }
                    screen == Screen.Settings -> SettingsScreen(
                        prefs = prefs,
                        model = model,
                        onTurnOnControl = { screen = Screen.Setup },
                        onKeepRunning = { screen = Screen.Setup },
                        onBack = home,
                    )
                    screen == Screen.Setup -> SetupScreen(prefs, model, onBack = home)
                    else -> Home(
                        model,
                        prefs,
                        onSettings = { screen = Screen.Settings },
                        onSetup = { screen = Screen.Setup },
                        onPair = { screen = Screen.Pair },
                    )
                }
            }
        }
    }
}

@Composable
private fun Home(
    model: HopwynModel,
    prefs: Prefs,
    onSettings: () -> Unit,
    onSetup: () -> Unit,
    onPair: () -> Unit,
) {
    val snackbar = remember { SnackbarHostState() }
    LaunchedEffect(model.message) {
        model.message?.let {
            snackbar.showSnackbar(it)
            model.message = null
        }
    }
    var forgetting by remember { mutableStateOf<Computer?>(null) }
    var addressing by remember { mutableStateOf<Computer?>(null) }
    val computers = model.computers
    val unpaired = model.nearby.filter { !it.paired }
    val muted = LocalHopwynColors.current.muted
    val network = Core.network

    Scaffold(
        containerColor = MaterialTheme.colorScheme.background,
        snackbarHost = { SnackbarHost(snackbar) },
        floatingActionButton = {
            ExtendedFloatingActionButton(
                onClick = onPair,
                icon = { Icon(Icons.Filled.Add, null) },
                text = { Text("Pair a computer") },
            )
        },
    ) { padding ->
        LazyColumn(
            modifier = Modifier.fillMaxSize().padding(padding),
            contentPadding = PaddingValues(start = 20.dp, end = 20.dp, top = 12.dp, bottom = 104.dp),
        ) {
            item {
                Row(Modifier.padding(top = 8.dp, bottom = 4.dp), verticalAlignment = Alignment.CenterVertically) {
                    Column(Modifier.weight(1f)) {
                        Text("Hopwyn", style = MaterialTheme.typography.headlineMedium)
                        if (model.deviceName.isNotEmpty()) {
                            Text(model.deviceName, style = MaterialTheme.typography.bodyMedium, color = muted)
                        }
                    }
                    IconButton(onClick = onSettings) { Icon(Icons.Filled.SettingsIcon, "Settings") }
                }
            }

            model.startError?.let { err ->
                item {
                    Notice(
                        "Hopwyn couldn't start: $err. Close and reopen the app. If that doesn't help, restart the phone.",
                        urgent = true,
                    )
                }
            }
            if (network != NetworkKind.Local) {
                item {
                    Notice(
                        if (network == NetworkKind.None) {
                            "This phone is offline. Join the Wi-Fi your computers use."
                        } else {
                            "This phone is on mobile data. Join the Wi-Fi your computers use, or connect a computer to this phone's hotspot."
                        },
                    )
                }
            }
            if (prefs.controlWasOn && !model.controllable) {
                item {
                    Notice(
                        "Phone control is off. If you didn't turn it off, the phone stopped Hopwyn in the background.",
                        urgent = true,
                    ) {
                        Row(Modifier.padding(top = 4.dp)) {
                            TextButton(onClick = onSetup) { Text("Turn on again") }
                            TextButton(onClick = { prefs.noteControl(false) }) { Text("Leave it off") }
                        }
                    }
                }
            }

            item {
                DeskMap(
                    computers = computers,
                    onUse = { model.control(it.id) },
                    onPair = onPair,
                    modifier = Modifier.padding(vertical = 12.dp),
                )
            }
            item {
                Text(
                    hint(computers, model.controllable, network),
                    style = MaterialTheme.typography.bodyMedium,
                    color = muted,
                    modifier = Modifier.padding(bottom = 8.dp),
                )
            }

            val needControl = !model.controllable && !prefs.controlWasOn
            if (needControl || !model.background) {
                item { SectionTitle("Finish setup") }
                item {
                    Group {
                        if (needControl) {
                            ItemRow(
                                icon = Icons.Filled.Mouse,
                                title = "Control this phone",
                                text = "Use a computer's mouse and keyboard here.",
                            ) {
                                FilledTonalButton(onClick = onSetup, Modifier.padding(end = 8.dp)) { Text("Set up") }
                            }
                        }
                        if (needControl && !model.background) RowDivider()
                        if (!model.background) {
                            ItemRow(
                                icon = Icons.Filled.BatteryAlert,
                                title = "Keep Hopwyn running",
                                text = "So the phone doesn't stop it and disconnect.",
                            ) {
                                FilledTonalButton(onClick = onSetup, Modifier.padding(end = 8.dp)) { Text("Set up") }
                            }
                        }
                    }
                }
            }

            if (computers.isNotEmpty()) {
                item { SectionTitle("Computers") }
                item {
                    Group {
                        computers.forEachIndexed { i, c ->
                            if (i > 0) RowDivider()
                            ComputerRow(
                                c,
                                network,
                                onUse = { model.control(c.id) },
                                onAddress = { addressing = c },
                                onForget = { forgetting = c },
                            )
                        }
                    }
                }
            }

            if (unpaired.isNotEmpty()) {
                item { SectionTitle("Nearby") }
                item {
                    Group {
                        unpaired.forEachIndexed { i, n ->
                            if (i > 0) RowDivider()
                            ItemRow(
                                icon = Icons.Filled.Laptop,
                                title = n.name,
                                text = if (n.open) "Ready to pair" else "Click Add computer on it to pair",
                            ) {
                                if (n.open) {
                                    FilledTonalButton(onClick = { model.join(n.id) }, Modifier.padding(end = 8.dp)) {
                                        Text("Pair")
                                    }
                                }
                            }
                        }
                    }
                }
            }

            item {
                Row(Modifier.fillMaxWidth().padding(top = 28.dp), verticalAlignment = Alignment.CenterVertically) {
                    Icon(Icons.Filled.Lock, null, Modifier.size(14.dp), tint = muted)
                    Spacer(Modifier.width(6.dp))
                    Text(
                        "Encrypted, on your own network only. No account, no cloud.",
                        style = MaterialTheme.typography.bodySmall,
                        color = muted,
                    )
                }
            }
        }
    }

    forgetting?.let { c ->
        AlertDialog(
            onDismissRequest = { forgetting = null },
            title = { Text("Forget ${c.name}?") },
            text = { Text("It won't be able to connect until you pair it again.") },
            confirmButton = {
                TextButton(onClick = { model.forget(c.id); forgetting = null }) { Text("Forget") }
            },
            dismissButton = { TextButton(onClick = { forgetting = null }) { Text("Cancel") } },
        )
    }
    addressing?.let { c ->
        AddressDialog(c, onSave = { model.setAddress(c.id, it); addressing = null }, onClose = { addressing = null })
    }
}

/** One line under the map saying what to do next. */
private fun hint(computers: List<Computer>, controllable: Boolean, network: NetworkKind): String = when {
    computers.isEmpty() ->
        "Pair a computer to start. On the computer, open Hopwyn and click Add computer."
    computers.none { it.online } && network != NetworkKind.Local ->
        "Your computers can't be reached without Wi-Fi."
    computers.none { it.online } && computers.any { it.seen && !it.dropping } -> "Connecting to your computer…"
    computers.none { it.online } ->
        "Your computers aren't on this network, or Hopwyn isn't open on them. Hopwyn connects on its own when one appears."
    controllable ->
        "Tap a computer to use this phone as its trackpad. Or move the computer's pointer off its screen edge to control this phone."
    else -> "Tap a computer to use this phone as its trackpad."
}

@Composable
private fun ComputerRow(
    c: Computer,
    network: NetworkKind,
    onUse: () -> Unit,
    onAddress: () -> Unit,
    onForget: () -> Unit,
) {
    val extra = LocalHopwynColors.current
    var menu by remember { mutableStateOf(false) }
    ItemRow(
        icon = Icons.Filled.Laptop,
        title = c.name,
        text = when {
            c.dropping -> "Keeps dropping the connection. Update Hopwyn on it, or forget it here and pair again."
            c.online -> "Connected"
            c.seen -> "Connecting…"
            network != NetworkKind.Local -> "Not connected"
            else -> "Not found on this network"
        },
        dot = if (c.online) extra.online else extra.line,
    ) {
        if (c.online) Button(onClick = onUse) { Text("Use") }
        Box {
            IconButton(onClick = { menu = true }) { Icon(Icons.Filled.MoreVert, "More for ${c.name}") }
            DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                DropdownMenuItem(text = { Text("Set address") }, onClick = { menu = false; onAddress() })
                DropdownMenuItem(text = { Text("Forget") }, onClick = { menu = false; onForget() })
            }
        }
    }
}

/** Where to look for a computer that a network hides. */
@Composable
private fun AddressDialog(c: Computer, onSave: (String) -> Unit, onClose: () -> Unit) {
    var address by remember { mutableStateOf(c.address) }
    AlertDialog(
        onDismissRequest = onClose,
        title = { Text("Address of ${c.name}") },
        text = {
            Column {
                Text(
                    "Hopwyn finds computers on its own on most networks. Where it can't, it tries this address. The computer shows its address under Add computer.",
                    style = MaterialTheme.typography.bodyMedium,
                )
                Spacer(Modifier.height(16.dp))
                OutlinedTextField(
                    value = address,
                    onValueChange = { v -> address = v.filter { it.isDigit() || it == '.' }.take(15) },
                    label = { Text("Address") },
                    placeholder = { Text("192.168.1.20") },
                    singleLine = true,
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal),
                    modifier = Modifier.fillMaxWidth(),
                )
            }
        },
        confirmButton = {
            TextButton(onClick = { onSave(address) }, enabled = looksLikeAddress(address)) { Text("Save") }
        },
        dismissButton = { TextButton(onClick = onClose) { Text("Cancel") } },
    )
}

/** Back closes a screen (and, for the trackpad, gives the computer its mouse back). */
@Composable
fun ReleaseOnBack(onDone: () -> Unit) = BackHandler(onBack = onDone)
