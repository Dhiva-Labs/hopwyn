package app.hopwyn.android

import android.content.Intent
import android.net.Uri
import android.os.Build
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Slider
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import kotlin.math.roundToInt

const val PRIVACY_URL = "https://github.com/Dhiva-Labs/hopwyn/blob/main/docs/privacy-policy.md"
const val SOURCE_URL = "https://github.com/Dhiva-Labs/hopwyn"

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(
    prefs: Prefs,
    model: HopwynModel,
    onTurnOnControl: () -> Unit,
    onKeepRunning: () -> Unit,
    onBack: () -> Unit,
) {
    ReleaseOnBack(onBack)
    val context = LocalContext.current
    val muted = LocalHopwynColors.current.muted
    val open = { url: String ->
        context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    }
    val version = remember(context) {
        runCatching { context.packageManager.getPackageInfo(context.packageName, 0).versionName }.getOrNull() ?: ""
    }

    Scaffold(
        containerColor = MaterialTheme.colorScheme.background,
        topBar = {
            TopAppBar(
                title = { Text("Settings") },
                navigationIcon = {
                    IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") }
                },
                colors = TopAppBarDefaults.topAppBarColors(containerColor = MaterialTheme.colorScheme.background),
            )
        },
    ) { padding ->
        LazyColumn(
            Modifier.fillMaxSize().padding(padding),
            contentPadding = PaddingValues(start = 20.dp, end = 20.dp, bottom = 32.dp),
        ) {
            item { Heading("Appearance") }
            item {
                Panel(Modifier.selectableGroup()) {
                    ThemeMode.entries.forEachIndexed { i, mode ->
                        if (i > 0) Divider()
                        Row(
                            Modifier
                                .fillMaxWidth()
                                .selectable(
                                    selected = prefs.theme == mode,
                                    onClick = { prefs.updateTheme(mode) },
                                    role = Role.RadioButton,
                                )
                                .padding(horizontal = 16.dp, vertical = 12.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Text(mode.label, Modifier.weight(1f), style = MaterialTheme.typography.bodyLarge)
                            RadioButton(selected = prefs.theme == mode, onClick = null)
                        }
                    }
                }
            }
            if (Build.VERSION.SDK_INT >= 31) {
                item {
                    Panel(Modifier.padding(top = 12.dp)) {
                        SwitchRow(
                            title = "Wallpaper colours",
                            text = "Use colours from your wallpaper instead of Hopwyn's.",
                            checked = prefs.wallpaperColors,
                            onChange = prefs::updateWallpaperColors,
                        )
                    }
                }
            }

            item { Heading("Trackpad") }
            item {
                Panel {
                    Column(Modifier.padding(horizontal = 16.dp, vertical = 12.dp)) {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Text("Pointer speed", Modifier.weight(1f), style = MaterialTheme.typography.bodyLarge)
                            Text(
                                "${(prefs.pointerSpeed / Prefs.DEFAULT_SPEED * 100).roundToInt()}%",
                                style = MaterialTheme.typography.bodyMedium,
                                color = muted,
                            )
                        }
                        Slider(
                            value = prefs.pointerSpeed,
                            onValueChange = prefs::updatePointerSpeed,
                            valueRange = 0.6f..3.2f,
                        )
                    }
                    Divider()
                    SwitchRow(
                        title = "Natural scrolling",
                        text = "Content moves with your fingers, as on the phone.",
                        checked = prefs.naturalScroll,
                        onChange = prefs::updateNaturalScroll,
                    )
                }
            }

            item { Heading("Clipboard") }
            item {
                Panel {
                    SwitchRow(
                        title = "Share the clipboard",
                        text = "Text copied on a paired computer arrives here. Text copied here is sent when you open Hopwyn. Turn it on on the computer too.",
                        checked = prefs.shareClipboard,
                        onChange = prefs::updateShareClipboard,
                    )
                }
            }

            item { Heading("This phone") }
            item {
                Panel {
                    InfoRow("Name", model.deviceName.ifEmpty { deviceName(context) }, "Change it in the phone's Settings, under About phone.")
                    Divider()
                    LinkRow(
                        title = "Control from a computer",
                        text = if (model.controllable) "On" else "Off. Turn on Hopwyn in Accessibility.",
                        onClick = onTurnOnControl,
                    )
                    Divider()
                    LinkRow(
                        title = "Keep running in the background",
                        text = if (model.background) "On" else "Off. The phone may stop Hopwyn and disconnect.",
                        onClick = onKeepRunning,
                    )
                }
            }

            item { Heading("About") }
            item {
                Panel {
                    InfoRow("Version", version, null)
                    Divider()
                    LinkRow("Privacy policy", "Hopwyn collects no data. Everything stays on your network.") { open(PRIVACY_URL) }
                    Divider()
                    LinkRow("Source code", "Open source under the Apache License 2.0.") { open(SOURCE_URL) }
                }
            }
        }
    }
}

@Composable
private fun remember(key: Any, calc: () -> String): String = androidx.compose.runtime.remember(key, calc)

@Composable
private fun Heading(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.titleSmall,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(start = 4.dp, top = 24.dp, bottom = 8.dp),
    )
}

@Composable
private fun Panel(modifier: Modifier = Modifier, content: @Composable () -> Unit) {
    Column(
        modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(20.dp))
            .background(MaterialTheme.colorScheme.surfaceVariant),
    ) { content() }
}

@Composable
private fun Divider() = HorizontalDivider(color = LocalHopwynColors.current.line)

@Composable
private fun SwitchRow(title: String, text: String, checked: Boolean, onChange: (Boolean) -> Unit) {
    Row(
        Modifier
            .fillMaxWidth()
            .toggleable(value = checked, onValueChange = onChange, role = Role.Switch)
            .padding(horizontal = 16.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f)) {
            Text(title, style = MaterialTheme.typography.bodyLarge)
            Text(text, style = MaterialTheme.typography.bodyMedium, color = LocalHopwynColors.current.muted)
        }
        Spacer(Modifier.width(16.dp))
        Switch(checked = checked, onCheckedChange = null)
    }
}

@Composable
private fun InfoRow(title: String, value: String, note: String?) {
    Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp)) {
        Row {
            Text(title, Modifier.weight(1f), style = MaterialTheme.typography.bodyLarge)
            Text(value, style = MaterialTheme.typography.bodyLarge, color = LocalHopwynColors.current.muted)
        }
        if (note != null) {
            Text(note, style = MaterialTheme.typography.bodySmall, color = LocalHopwynColors.current.muted)
        }
    }
}

@Composable
private fun LinkRow(title: String, text: String, onClick: () -> Unit) {
    Column(
        Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 12.dp),
    ) {
        Text(title, style = MaterialTheme.typography.bodyLarge)
        Text(text, style = MaterialTheme.typography.bodyMedium, color = LocalHopwynColors.current.muted)
    }
}
