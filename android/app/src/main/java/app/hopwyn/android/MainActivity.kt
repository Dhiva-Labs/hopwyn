package app.hopwyn.android

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.viewModels
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
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
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.BatteryAlert
import androidx.compose.material.icons.filled.Computer
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material.icons.filled.Mouse
import androidx.compose.material.icons.filled.TouchApp
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

class MainActivity : ComponentActivity() {
    private val model: HopwynModel by viewModels()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent {
            HopwynTheme {
                val controlling = model.controlling
                if (controlling != null) {
                    val name = model.computers.firstOrNull { it.id == controlling }?.name ?: "computer"
                    Trackpad(name = name, onDone = model::release)
                } else {
                    Home(model)
                }
                PairingDialogs(model)
            }
        }
    }
}

private val HopwynBlue = Color(0xFF3B78F0)

@Composable
fun HopwynTheme(content: @Composable () -> Unit) {
    val dark = isSystemInDarkTheme()
    val context = LocalContext.current
    val colors = when {
        android.os.Build.VERSION.SDK_INT >= 31 ->
            if (dark) dynamicDarkColorScheme(context) else dynamicLightColorScheme(context)
        dark -> darkColorScheme(primary = HopwynBlue)
        else -> lightColorScheme(primary = HopwynBlue)
    }
    MaterialTheme(colorScheme = colors, content = content)
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun Home(model: HopwynModel) {
    val snackbar = remember { SnackbarHostState() }
    LaunchedEffect(model.message) {
        model.message?.let {
            snackbar.showSnackbar(it)
            model.message = null
        }
    }
    var forgetting by remember { mutableStateOf<Computer?>(null) }

    Scaffold(
        topBar = {
            TopAppBar(title = {
                Column {
                    Text("Hopwyn", fontWeight = FontWeight.SemiBold)
                    if (model.deviceName.isNotEmpty()) {
                        Text(
                            model.deviceName,
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            })
        },
        snackbarHost = { SnackbarHost(snackbar) },
    ) { padding ->
        LazyColumn(
            modifier = Modifier.fillMaxSize().padding(padding),
            contentPadding = PaddingValues(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            model.startError?.let { err ->
                item {
                    Card { Text("Hopwyn couldn't start: $err", Modifier.padding(16.dp)) }
                }
            }
            item {
                Card(Modifier.fillMaxWidth()) {
                    Column(Modifier.padding(20.dp), horizontalAlignment = Alignment.CenterHorizontally) {
                        Icon(Icons.Filled.TouchApp, null, Modifier.size(48.dp), tint = MaterialTheme.colorScheme.primary)
                        Spacer(Modifier.size(8.dp))
                        Text("Your phone as a trackpad and keyboard", style = MaterialTheme.typography.titleMedium)
                        Text(
                            "Pick a paired computer and tap Use. Everything stays on your home network, encrypted.",
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }

            item { ControlCard(model.controllable) }
            if (!model.background) item { BackgroundCard() }

            item { SectionTitle("Paired computers") }
            if (model.computers.isEmpty()) {
                item {
                    Text(
                        "None yet. Pair one below.",
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(horizontal = 4.dp),
                    )
                }
            }
            items(model.computers, key = { it.id }) { c ->
                Card {
                    ListItem(
                        leadingContent = { Icon(Icons.Filled.Computer, null) },
                        headlineContent = { Text(c.name) },
                        supportingContent = { Text(if (c.online) "Connected" else "Not connected") },
                        trailingContent = {
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Button(onClick = { model.control(c.id) }, enabled = c.online) { Text("Use") }
                                IconButton(onClick = { forgetting = c }) {
                                    Icon(Icons.Filled.Delete, "Forget ${c.name}")
                                }
                            }
                        },
                    )
                }
            }

            item { SectionTitle("Add a computer") }
            item {
                Card {
                    Column(Modifier.padding(16.dp)) {
                        Text(
                            "On the computer, click Add computer: this phone asks on its own. Or tap below and the computer asks. Both show the same code; tap Pair on each.",
                            style = MaterialTheme.typography.bodyMedium,
                        )
                        Spacer(Modifier.size(12.dp))
                        FilledTonalButton(onClick = model::addComputer) { Text("Pair a computer") }
                    }
                }
            }

            val unpaired = model.nearby.filter { !it.paired }
            if (unpaired.isNotEmpty()) {
                item { SectionTitle("Nearby") }
                items(unpaired, key = { "n" + it.id }) { n ->
                    Card {
                        ListItem(
                            leadingContent = { Icon(Icons.Filled.Computer, null) },
                            headlineContent = { Text(n.name) },
                            supportingContent = {
                                Text(if (n.open) "Ready to pair" else "Click Add computer on it to pair")
                            },
                            trailingContent = {
                                if (n.open) Button(onClick = { model.join(n.id) }) { Text("Pair") }
                            },
                        )
                    }
                }
            }

            item {
                Row(
                    Modifier.fillMaxWidth().padding(top = 8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.Center,
                ) {
                    Icon(Icons.Filled.Lock, null, Modifier.size(16.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
                    Spacer(Modifier.width(6.dp))
                    Text(
                        "No account, no cloud. Home network only.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }
    }

    forgetting?.let { c ->
        AlertDialog(
            onDismissRequest = { forgetting = null },
            title = { Text("Forget ${c.name}?") },
            text = { Text("It will no longer be able to connect. You can pair again later.") },
            confirmButton = {
                TextButton(onClick = { model.forget(c.id); forgetting = null }) { Text("Forget") }
            },
            dismissButton = { TextButton(onClick = { forgetting = null }) { Text("Cancel") } },
        )
    }
}

/** Status of "computers can control this phone", with a way to turn it on. */
@Composable
private fun ControlCard(on: Boolean) {
    val context = LocalContext.current
    Card(Modifier.fillMaxWidth()) {
        ListItem(
            leadingContent = { Icon(Icons.Filled.Mouse, null) },
            headlineContent = { Text("Control this phone from a computer") },
            supportingContent = {
                Text(
                    if (on) {
                        "On: move the computer's pointer off the edge where this phone sits."
                    } else {
                        "Turn on Hopwyn in Accessibility settings to use your computer's mouse and keyboard here. It also keeps Hopwyn connected when closed."
                    },
                )
            },
            trailingContent = {
                if (!on) {
                    Button(onClick = {
                        context.startActivity(
                            android.content.Intent(android.provider.Settings.ACTION_ACCESSIBILITY_SETTINGS)
                                .addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK),
                        )
                    }) { Text("Turn on") }
                }
            },
        )
    }
}

/** Asks Android, once, to stop putting Hopwyn to sleep in the background. */
@Composable
private fun BackgroundCard() {
    val context = LocalContext.current
    Card(Modifier.fillMaxWidth()) {
        ListItem(
            leadingContent = { Icon(Icons.Filled.BatteryAlert, null) },
            headlineContent = { Text("Keep Hopwyn running") },
            supportingContent = {
                Text(
                    "The phone may stop Hopwyn in the background, which disconnects it and turns phone control off. Allow it to keep running.",
                )
            },
            trailingContent = {
                Button(onClick = {
                    val ask = android.content.Intent(
                        android.provider.Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS,
                        android.net.Uri.parse("package:${context.packageName}"),
                    ).addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK)
                    try {
                        context.startActivity(ask)
                    } catch (_: android.content.ActivityNotFoundException) {
                        context.startActivity(
                            android.content.Intent(android.provider.Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS)
                                .addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK),
                        )
                    }
                }) { Text("Allow") }
            },
        )
    }
}

@Composable
private fun SectionTitle(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.titleSmall,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(start = 4.dp, top = 8.dp),
    )
}

@Composable
fun PairingDialogs(model: HopwynModel) {
    when (val p = model.pairing) {
        PairingState.None -> {}
        PairingState.Open -> AlertDialog(
            onDismissRequest = {},
            title = { Text("Waiting for a computer") },
            text = {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    CircularProgressIndicator(Modifier.size(28.dp))
                    Spacer(Modifier.width(16.dp))
                    Text("Open Hopwyn on the computer. It will show a pairing code on its own within a few seconds.")
                }
            },
            confirmButton = {},
            dismissButton = { TextButton(onClick = model::cancelPairing) { Text("Cancel") } },
        )
        is PairingState.Confirm -> AlertDialog(
            onDismissRequest = {},
            title = { Text("Pair with ${p.name}?") },
            text = {
                Column(horizontalAlignment = Alignment.CenterHorizontally, modifier = Modifier.fillMaxWidth()) {
                    Text("Check that ${p.name} shows the same code, then tap Pair on both.")
                    Spacer(Modifier.size(16.dp))
                    Text(
                        p.code.take(3) + " " + p.code.drop(3),
                        fontSize = 40.sp,
                        fontWeight = FontWeight.ExtraBold,
                        letterSpacing = 4.sp,
                    )
                }
            },
            confirmButton = { Button(onClick = { model.confirm(true) }) { Text("Pair") } },
            dismissButton = { TextButton(onClick = { model.confirm(false) }) { Text("Cancel") } },
        )
        is PairingState.Waiting -> AlertDialog(
            onDismissRequest = {},
            title = { Text("Waiting for ${p.name}") },
            text = { Text("Click Pair on ${p.name} too.") },
            confirmButton = {},
            dismissButton = { TextButton(onClick = model::cancelPairing) { Text("Cancel") } },
        )
    }
}

/** Back closes the trackpad and gives the computer its own mouse back. */
@Composable
fun ReleaseOnBack(onDone: () -> Unit) = BackHandler(onBack = onDone)
