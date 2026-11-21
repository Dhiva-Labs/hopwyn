package app.hopwyn.android

import android.content.ActivityNotFoundException
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.provider.Settings
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.activity.SystemBarStyle
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.graphics.luminance
import androidx.activity.viewModels
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
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
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
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
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp

class MainActivity : ComponentActivity() {
    private val model: HopwynModel by viewModels()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val prefs = Prefs.get(this)
        setContent {
            HopwynTheme(prefs) {
                val controlling = model.controlling
                var settings by rememberSaveable { mutableStateOf(false) }
                var disclosing by remember { mutableStateOf(false) }
                val turnOnControl = {
                    if (prefs.controlDisclosed) openAccessibility(this) else disclosing = true
                }
                val keepRunning = { openBatterySettings(this) }

                // System bar icons follow the screen shown, not the phone's theme.
                val dark = controlling != null || MaterialTheme.colorScheme.background.luminance() < 0.5f
                LaunchedEffect(dark) {
                    val clear = android.graphics.Color.TRANSPARENT
                    val style = if (dark) SystemBarStyle.dark(clear) else SystemBarStyle.light(clear, clear)
                    enableEdgeToEdge(statusBarStyle = style, navigationBarStyle = style)
                }

                when {
                    controlling != null -> {
                        val name = model.computers.firstOrNull { it.id == controlling }?.name ?: "computer"
                        Trackpad(name = name, prefs = prefs, onDone = model::release)
                    }
                    settings -> SettingsScreen(
                        prefs = prefs,
                        model = model,
                        onTurnOnControl = turnOnControl,
                        onKeepRunning = keepRunning,
                        onBack = { settings = false },
                    )
                    else -> Home(
                        model,
                        onSettings = { settings = true },
                        onTurnOnControl = turnOnControl,
                        onKeepRunning = keepRunning,
                    )
                }
                PairingDialogs(model)
                if (disclosing) {
                    ControlDisclosure(
                        onAgree = {
                            disclosing = false
                            prefs.agreeToControl()
                            openAccessibility(this)
                        },
                        onDecline = { disclosing = false },
                    )
                }
            }
        }
    }
}

private fun openAccessibility(context: android.content.Context) =
    context.startActivity(Intent(Settings.ACTION_ACCESSIBILITY_SETTINGS).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))

/** Hopwyn's App info page, where Battery has the "Unrestricted" choice. */
private fun openBatterySettings(context: android.content.Context) {
    val info = Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.parse("package:${context.packageName}"))
        .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
    try {
        context.startActivity(info)
    } catch (_: ActivityNotFoundException) {
        context.startActivity(
            Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
        )
    }
}

/**
 * Says plainly what phone control can see and do before sending the user to
 * Accessibility settings, as Google Play requires for this permission.
 */
@Composable
private fun ControlDisclosure(onAgree: () -> Unit, onDecline: () -> Unit) {
    AlertDialog(
        onDismissRequest = onDecline,
        icon = { Icon(Icons.Filled.Mouse, null) },
        title = { Text("Let computers control this phone?") },
        text = {
            Column {
                Text(
                    "Hopwyn uses Android's Accessibility service so that a computer you paired can use its mouse and keyboard on this phone.",
                )
                Spacer(Modifier.size(12.dp))
                Text("With it on, Hopwyn:", style = MaterialTheme.typography.titleSmall)
                Text(
                    "• shows a pointer and turns clicks, drags and scrolling into taps and swipes\n" +
                        "• presses Back and Home\n" +
                        "• types into the text field you have selected, which means finding that field on screen",
                )
                Spacer(Modifier.size(12.dp))
                Text(
                    "It doesn't read, store or send what is on your screen. Nothing leaves your home network, and it only works while a paired computer is connected. You can turn it off any time in Accessibility settings.",
                )
            }
        },
        confirmButton = { Button(onClick = onAgree) { Text("Agree and turn on") } },
        dismissButton = { TextButton(onClick = onDecline) { Text("Not now") } },
    )
}

@Composable
fun Home(
    model: HopwynModel,
    onSettings: () -> Unit,
    onTurnOnControl: () -> Unit,
    onKeepRunning: () -> Unit,
) {
    val snackbar = remember { SnackbarHostState() }
    LaunchedEffect(model.message) {
        model.message?.let {
            snackbar.showSnackbar(it)
            model.message = null
        }
    }
    var forgetting by remember { mutableStateOf<Computer?>(null) }
    val computers = model.computers
    val unpaired = model.nearby.filter { !it.paired }
    val muted = LocalHopwynColors.current.muted

    Scaffold(
        containerColor = MaterialTheme.colorScheme.background,
        snackbarHost = { SnackbarHost(snackbar) },
        floatingActionButton = {
            ExtendedFloatingActionButton(
                onClick = model::addComputer,
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
                    Notice("Hopwyn couldn't start: $err. Close and reopen the app. If that doesn't help, restart the phone.")
                }
            }

            item {
                DeskMap(
                    computers = computers,
                    onUse = { model.control(it.id) },
                    onPair = model::addComputer,
                    modifier = Modifier.padding(vertical = 12.dp),
                )
            }
            item { Hint(hint(computers, model.controllable)) }

            if (!model.controllable || !model.background) {
                item { SectionTitle("Finish setup") }
                item {
                    Group {
                        if (!model.controllable) ControlStep(onTurnOnControl)
                        if (!model.controllable && !model.background) RowDivider()
                        if (!model.background) BackgroundStep(onKeepRunning)
                    }
                }
            }

            if (computers.isNotEmpty()) {
                item { SectionTitle("Computers") }
                item {
                    Group {
                        computers.forEachIndexed { i, c ->
                            if (i > 0) RowDivider()
                            ComputerRow(c, onUse = { model.control(c.id) }, onForget = { forgetting = c })
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
                                if (n.open) FilledTonalButton(onClick = { model.join(n.id) }) { Text("Pair") }
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
                        "Encrypted, on your home network only. No account, no cloud.",
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
}

/** One line under the map saying what to do next. */
private fun hint(computers: List<Computer>, controllable: Boolean): String = when {
    computers.isEmpty() ->
        "Pair a computer to start. On the computer, open Hopwyn and click Add computer: this phone asks on its own."
    computers.none { it.online } ->
        "Your computers aren't connected. Open Hopwyn on one, on the same Wi-Fi as this phone."
    controllable ->
        "Tap a computer to use this phone as its trackpad. Or move the computer's pointer off its screen edge to control this phone."
    else -> "Tap a computer to use this phone as its trackpad."
}

@Composable
private fun Hint(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.bodyMedium,
        color = LocalHopwynColors.current.muted,
        modifier = Modifier.padding(bottom = 8.dp),
    )
}

@Composable
private fun Notice(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onErrorContainer,
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 8.dp)
            .clip(RoundedCornerShape(16.dp))
            .background(MaterialTheme.colorScheme.errorContainer)
            .padding(16.dp),
    )
}

@Composable
private fun SectionTitle(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.titleSmall,
        color = MaterialTheme.colorScheme.onBackground,
        modifier = Modifier.padding(start = 4.dp, top = 24.dp, bottom = 8.dp),
    )
}

/** Rows that belong together, on one raised surface. */
@Composable
private fun Group(content: @Composable () -> Unit) {
    Column(
        Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(20.dp))
            .background(MaterialTheme.colorScheme.surfaceVariant),
    ) { content() }
}

@Composable
private fun RowDivider() =
    HorizontalDivider(Modifier.padding(start = 60.dp), color = LocalHopwynColors.current.line)

@Composable
private fun ItemRow(
    icon: ImageVector,
    title: String,
    text: String,
    dot: Color? = null,
    trailing: @Composable () -> Unit = {},
) {
    Row(
        Modifier.fillMaxWidth().padding(start = 16.dp, end = 8.dp, top = 14.dp, bottom = 14.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(icon, null, Modifier.size(24.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
        Spacer(Modifier.width(20.dp))
        Column(Modifier.weight(1f)) {
            Text(title, style = MaterialTheme.typography.titleMedium, maxLines = 2, overflow = TextOverflow.Ellipsis)
            Row(verticalAlignment = Alignment.CenterVertically) {
                if (dot != null) {
                    Box(Modifier.size(8.dp).clip(CircleShape).background(dot))
                    Spacer(Modifier.width(6.dp))
                }
                Text(text, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
        Spacer(Modifier.width(8.dp))
        trailing()
    }
}

@Composable
private fun ComputerRow(c: Computer, onUse: () -> Unit, onForget: () -> Unit) {
    val extra = LocalHopwynColors.current
    var menu by remember { mutableStateOf(false) }
    ItemRow(
        icon = Icons.Filled.Laptop,
        title = c.name,
        text = if (c.online) "Connected" else "Not connected",
        dot = if (c.online) extra.online else extra.line,
    ) {
        if (c.online) Button(onClick = onUse) { Text("Use") }
        Box {
            IconButton(onClick = { menu = true }) { Icon(Icons.Filled.MoreVert, "More for ${c.name}") }
            DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                DropdownMenuItem(text = { Text("Forget") }, onClick = { menu = false; onForget() })
            }
        }
    }
}

@Composable
private fun ControlStep(onTurnOn: () -> Unit) {
    ItemRow(
        icon = Icons.Filled.Mouse,
        title = "Control this phone",
        text = "Use a computer's mouse and keyboard here.",
    ) {
        FilledTonalButton(onClick = onTurnOn) { Text("Turn on") }
    }
}

@Composable
private fun BackgroundStep(onOpen: () -> Unit) {
    ItemRow(
        icon = Icons.Filled.BatteryAlert,
        title = "Keep Hopwyn running",
        text = "Open Battery and choose Unrestricted, so the phone doesn't stop it and disconnect.",
    ) {
        FilledTonalButton(onClick = onOpen) { Text("Open") }
    }
}

@Composable
fun PairingDialogs(model: HopwynModel) {
    when (val p = model.pairing) {
        PairingState.None -> {}
        PairingState.Open -> AlertDialog(
            onDismissRequest = {},
            icon = { CircularProgressIndicator(Modifier.size(32.dp), strokeWidth = 3.dp) },
            title = { Text("Waiting for a computer") },
            text = { Text("Open Hopwyn on the computer, on the same Wi-Fi. It shows a pairing code within a few seconds.") },
            confirmButton = {},
            dismissButton = { TextButton(onClick = model::cancelPairing) { Text("Cancel") } },
        )
        is PairingState.Confirm -> AlertDialog(
            onDismissRequest = {},
            title = { Text("Pair with ${p.name}?") },
            text = {
                Column(Modifier.fillMaxWidth(), horizontalAlignment = Alignment.CenterHorizontally) {
                    Text("Pair only if ${p.name} shows this same code.", style = MaterialTheme.typography.bodyMedium)
                    Spacer(Modifier.size(20.dp))
                    Text(
                        p.code.take(3) + " " + p.code.drop(3),
                        style = CodeStyle,
                        color = MaterialTheme.colorScheme.onPrimaryContainer,
                        modifier = Modifier
                            .clip(RoundedCornerShape(16.dp))
                            .background(MaterialTheme.colorScheme.primaryContainer)
                            .padding(horizontal = 20.dp, vertical = 8.dp),
                    )
                }
            },
            confirmButton = { Button(onClick = { model.confirm(true) }) { Text("Pair") } },
            dismissButton = { TextButton(onClick = { model.confirm(false) }) { Text("Cancel") } },
        )
        is PairingState.Waiting -> AlertDialog(
            onDismissRequest = {},
            icon = { CircularProgressIndicator(Modifier.size(32.dp), strokeWidth = 3.dp) },
            title = { Text("Waiting for ${p.name}") },
            text = { Text("Click Pair on ${p.name} to finish.") },
            confirmButton = {},
            dismissButton = { TextButton(onClick = model::cancelPairing) { Text("Cancel") } },
        )
    }
}

/** Back closes the trackpad and gives the computer its own mouse back. */
@Composable
fun ReleaseOnBack(onDone: () -> Unit) = BackHandler(onBack = onDone)
