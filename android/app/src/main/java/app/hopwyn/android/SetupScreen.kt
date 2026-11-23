package app.hopwyn.android

import android.content.ActivityNotFoundException
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.provider.Settings
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
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material3.Button
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp

/** Opens Accessibility settings, at Hopwyn's entry where the phone allows. */
fun openAccessibility(context: Context) {
    val service = ComponentName(context, ControlService::class.java).flattenToString()
    val settings = Intent(Settings.ACTION_ACCESSIBILITY_SETTINGS)
        .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        .putExtra(":settings:fragment_args_key", service)
        .putExtra(":settings:show_fragment_args", Bundle().apply { putString(":settings:fragment_args_key", service) })
    ControlService.returnWhenOn()
    open(context, settings, Intent(Settings.ACTION_SETTINGS))
}

/** Hopwyn's App info page: Battery, and "Allow restricted settings". */
fun openAppInfo(context: Context) = open(
    context,
    Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.parse("package:${context.packageName}")),
    Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS),
)

/** Starts the first of [intents] the phone has a screen for. */
private fun open(context: Context, vararg intents: Intent) {
    for (intent in intents) {
        try {
            context.startActivity(intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
            return
        } catch (_: ActivityNotFoundException) {
        } catch (_: SecurityException) {
        }
    }
}

/**
 * The two things Android needs the user to switch on by hand, each with
 * the taps it takes. Says plainly what phone control can see and do before
 * sending the user to Accessibility settings, as Google Play requires.
 */
@Composable
fun SetupScreen(prefs: Prefs, model: HopwynModel, onBack: () -> Unit) {
    ReleaseOnBack(onBack)
    val context = LocalContext.current
    // Shown once the user has been to settings and come back without it on.
    var triedControl by rememberSaveable { mutableStateOf(false) }
    var triedBattery by rememberSaveable { mutableStateOf(false) }

    Scaffold(
        containerColor = MaterialTheme.colorScheme.background,
        topBar = { ScreenBar("Phone control", onBack) },
    ) { padding ->
        LazyColumn(
            Modifier.fillMaxSize().padding(padding),
            contentPadding = PaddingValues(start = 20.dp, end = 20.dp, bottom = 32.dp),
        ) {
            item { SectionTitle("Let computers control this phone") }
            item {
                Group {
                    if (model.controllable) {
                        Done("Phone control is on", "Move a paired computer's pointer off its screen edge to use it here.")
                    } else {
                        Column(Modifier.padding(16.dp)) {
                            Text(
                                "Hopwyn uses Android's Accessibility service so that a computer you paired can use its mouse and keyboard on this phone.",
                                style = MaterialTheme.typography.bodyMedium,
                            )
                            Spacer(Modifier.height(12.dp))
                            Text("With it on, Hopwyn:", style = MaterialTheme.typography.titleSmall)
                            Text(
                                "• shows a pointer and turns clicks, drags and scrolling into taps and swipes\n" +
                                    "• presses Back and Home\n" +
                                    "• types into the text field you have selected, which means finding that field on screen",
                                style = MaterialTheme.typography.bodyMedium,
                            )
                            Spacer(Modifier.height(12.dp))
                            Text(
                                "It doesn't read, store or send what is on your screen. Nothing leaves your network, and it only works while a paired computer is connected. You can turn it off any time in Accessibility settings.",
                                style = MaterialTheme.typography.bodyMedium,
                            )
                            Spacer(Modifier.height(16.dp))
                            Text("To turn it on", style = MaterialTheme.typography.titleSmall)
                            Step(1, "Tap the button below. Accessibility settings open.")
                            Step(2, "Tap Hopwyn. It may be under Installed apps or Downloaded apps.")
                            Step(3, "Turn on Use Hopwyn, then tap Allow. You come back here on your own.")
                            Spacer(Modifier.height(12.dp))
                            Button(
                                onClick = {
                                    prefs.agreeToControl()
                                    triedControl = true
                                    openAccessibility(context)
                                },
                                modifier = Modifier.fillMaxWidth(),
                            ) {
                                Text(if (prefs.controlDisclosed) "Open Accessibility settings" else "Agree and open settings")
                            }
                            if (triedControl) {
                                Notice(
                                    "Can't turn it on? If Hopwyn is greyed out and Android says \"Restricted setting\", open App info, tap the ⋮ menu at the top, choose Allow restricted settings, then try again.",
                                    Modifier.padding(top = 8.dp),
                                ) {
                                    TextButton(onClick = { openAppInfo(context) }) { Text("Open App info") }
                                }
                            }
                        }
                    }
                }
            }

            item { SectionTitle("Keep Hopwyn running") }
            item {
                Group {
                    if (model.background) {
                        Done("Hopwyn stays running", "The phone won't stop it in the background.")
                    } else {
                        Column(Modifier.padding(16.dp)) {
                            Text(
                                "Many phones stop apps that run in the background. When that happens to Hopwyn, phone control turns itself off and your computers disconnect.",
                                style = MaterialTheme.typography.bodyMedium,
                            )
                            Spacer(Modifier.height(16.dp))
                            Text("To stop that", style = MaterialTheme.typography.titleSmall)
                            Step(1, "Tap the button below. Hopwyn's App info opens.")
                            Step(2, "Tap Battery, or Battery usage.")
                            Step(3, "Choose Unrestricted, or turn on Allow background activity. Then come back.")
                            Spacer(Modifier.height(12.dp))
                            Button(
                                onClick = {
                                    triedBattery = true
                                    openAppInfo(context)
                                },
                                modifier = Modifier.fillMaxWidth(),
                            ) { Text("Open App info") }
                            if (triedBattery) {
                                Text(
                                    "Still shown as off? Some phones name it differently: look for Battery optimisation and choose Don't optimise.",
                                    style = MaterialTheme.typography.bodySmall,
                                    color = LocalHopwynColors.current.muted,
                                    modifier = Modifier.padding(top = 12.dp),
                                )
                            }
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun Done(title: String, text: String) {
    Row(Modifier.fillMaxWidth().padding(16.dp), verticalAlignment = Alignment.CenterVertically) {
        Icon(Icons.Filled.CheckCircle, null, Modifier.size(28.dp), tint = LocalHopwynColors.current.online)
        Spacer(Modifier.width(16.dp))
        Column {
            Text(title, style = MaterialTheme.typography.titleMedium)
            Text(text, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}
