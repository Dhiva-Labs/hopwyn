package app.hopwyn.android

import android.view.HapticFeedbackConstants
import androidx.compose.foundation.background
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.graphics.Brush
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.ui.draw.clip
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Keyboard
import androidx.compose.material3.AssistChip
import androidx.compose.material3.Button
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.PointerId
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.unit.dp

// The trackpad is always dark, like the real thing, whatever the phone's theme.
private val Pad = Color(0xFF0E131C)
private val Key = Color(0xFF1A212D)
private val Pressed = Color(0xFF2A3446)
private val Glow = Color(0x403B78F0)
private val Live = Color(0xFF5FD3A2)
private val Muted = Color(0xFF97A1B2)
private val Hint = Color(0xFF5E6878)

/** Finger pixels per scrolled line. */
private const val PX_PER_LINE = 28f
/** Longest touch that still counts as a tap, and how far it may move. */
private const val TAP_MS = 250L
private const val TAP_SLOP = 14f

/**
 * Full-screen trackpad: one finger moves, a tap clicks, a two-finger tap
 * right-clicks, two fingers scroll. "Hold" keeps the left button down for
 * dragging. The keyboard types on the computer.
 */
@Composable
fun Trackpad(name: String, prefs: Prefs, onDone: () -> Unit) {
    ReleaseOnBack(onDone)
    val view = LocalView.current
    DisposableEffect(Unit) {
        view.keepScreenOn = true
        onDispose { view.keepScreenOn = false }
    }
    var typing by remember { mutableStateOf(false) }

    Surface(color = Pad, modifier = Modifier.fillMaxSize()) {
        Column(Modifier.fillMaxSize().statusBarsPadding().navigationBarsPadding().imePadding()) {
            Row(
                Modifier.fillMaxWidth().padding(16.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(Modifier.weight(1f)) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Box(Modifier.size(8.dp).clip(CircleShape).background(Live))
                        Text(
                            "  Controlling",
                            color = Muted,
                            style = MaterialTheme.typography.labelLarge,
                        )
                    }
                    Text(
                        name,
                        color = Color.White,
                        style = MaterialTheme.typography.titleLarge,
                        maxLines = 1,
                        overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
                    )
                }
                Button(onClick = onDone) { Text("Done") }
            }

            TouchSurface(prefs, Modifier.weight(1f).fillMaxWidth().padding(horizontal = 12.dp))

            if (typing) KeyBar()

            Row(
                Modifier.fillMaxWidth().padding(12.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Row(
                    Modifier.weight(1f).height(56.dp).clip(RoundedCornerShape(18.dp)),
                    horizontalArrangement = Arrangement.spacedBy(2.dp),
                ) {
                    MouseButton("Left", 0, Modifier.weight(1f))
                    MouseButton("Right", 1, Modifier.weight(1f))
                }
                HoldButton(Modifier.height(56.dp))
                FilledTonalButton(
                    onClick = { typing = !typing },
                    modifier = Modifier.height(56.dp),
                    shape = RoundedCornerShape(18.dp),
                ) {
                    Icon(Icons.Filled.Keyboard, if (typing) "Hide keyboard" else "Show keyboard")
                }
            }
            if (typing) HiddenKeyboard()
        }
    }
}

@Composable
private fun TouchSurface(prefs: Prefs, modifier: Modifier) {
    val view = LocalView.current
    var touch by remember { mutableStateOf<Offset?>(null) }
    Box(
        modifier
            .clip(RoundedCornerShape(24.dp))
            .background(Key)
            .drawBehind {
                touch?.let {
                    drawCircle(
                        Brush.radialGradient(listOf(Glow, Color.Transparent), center = it, radius = 90.dp.toPx()),
                        radius = 90.dp.toPx(),
                        center = it,
                    )
                }
            }
            .pointerInput(Unit) {
                awaitEachGesture {
                    val down = awaitFirstDown(requireUnconsumed = false)
                    touch = down.position
                    val start = down.uptimeMillis
                    var end = start
                    var last: Map<PointerId, Offset> = mapOf(down.id to down.position)
                    var maxFingers = 1
                    var travelled = 0f
                    var scrollCarry = Offset.Zero
                    do {
                        val event = awaitPointerEvent()
                        val pressed = event.changes.filter { it.pressed }
                        maxFingers = maxOf(maxFingers, pressed.size)
                        if (pressed.size == 1 && maxFingers == 1) {
                            val c = pressed.first()
                            last[c.id]?.let { prev ->
                                val d = c.position - prev
                                travelled += d.getDistance()
                                val speed = prefs.pointerSpeed
                                Native.nativeMotion(d.x * speed, d.y * speed)
                            }
                        } else if (pressed.size >= 2) {
                            var sum = Offset.Zero
                            var n = 0
                            for (c in pressed) last[c.id]?.let { sum += c.position - it; n++ }
                            if (n > 0) {
                                val d = sum / n.toFloat()
                                travelled += d.getDistance()
                                // Natural scrolling moves content with the fingers.
                                scrollCarry += if (prefs.naturalScroll) d else -d
                                val lines = Offset(-scrollCarry.x / PX_PER_LINE, -scrollCarry.y / PX_PER_LINE)
                                val whole = Offset(lines.x.toInt().toFloat(), lines.y.toInt().toFloat())
                                if (whole != Offset.Zero) {
                                    Native.nativeScroll(whole.x, whole.y)
                                    scrollCarry -= Offset(-whole.x * PX_PER_LINE, -whole.y * PX_PER_LINE)
                                }
                            }
                        }
                        last = pressed.associate { it.id to it.position }
                        touch = pressed.firstOrNull()?.position
                        event.changes.forEach {
                            if (!it.pressed) end = it.uptimeMillis
                            it.consume()
                        }
                    } while (event.changes.any { it.pressed })
                    if (travelled < TAP_SLOP && end - start < TAP_MS) {
                        val button = if (maxFingers >= 2) 1 else 0
                        Native.nativeButton(button, true)
                        Native.nativeButton(button, false)
                        view.performHapticFeedback(HapticFeedbackConstants.CONTEXT_CLICK)
                    }
                    touch = null
                }
            },
        contentAlignment = Alignment.Center,
    ) {
        Text(
            "Move with one finger, tap to click.\nScroll with two fingers, tap with two to right-click.",
            color = Hint,
            textAlign = androidx.compose.ui.text.style.TextAlign.Center,
            style = MaterialTheme.typography.bodySmall,
        )
    }
}

@Composable
private fun MouseButton(label: String, button: Int, modifier: Modifier) {
    val view = LocalView.current
    var down by remember { mutableStateOf(false) }
    Box(
        modifier
            .fillMaxHeight()
            .background(if (down) Pressed else Key)
            .pointerInput(Unit) {
                detectTapGestures(onPress = {
                    down = true
                    Native.nativeButton(button, true)
                    view.performHapticFeedback(HapticFeedbackConstants.VIRTUAL_KEY)
                    tryAwaitRelease()
                    Native.nativeButton(button, false)
                    down = false
                })
            },
        contentAlignment = Alignment.Center,
    ) { Text(label, color = Color.White, style = MaterialTheme.typography.labelLarge) }
}

/** Toggles the left button held, for dragging with one finger. */
@Composable
private fun HoldButton(modifier: Modifier) {
    var held by remember { mutableStateOf(false) }
    val toggle = {
        held = !held
        Native.nativeButton(0, held)
    }
    if (held) {
        Button(onClick = toggle, modifier = modifier, shape = RoundedCornerShape(18.dp)) { Text("Drop") }
    } else {
        OutlinedButton(onClick = toggle, modifier = modifier, shape = RoundedCornerShape(18.dp)) {
            Text("Hold", color = Color.White)
        }
    }
    DisposableEffect(Unit) { onDispose { if (held) Native.nativeButton(0, false) } }
}

/** Keys the phone keyboard doesn't have. */
@Composable
private fun KeyBar() {
    val keys = listOf(
        "Esc" to Hid.ESCAPE, "Tab" to Hid.TAB, "←" to Hid.LEFT, "↑" to Hid.UP,
        "↓" to Hid.DOWN, "→" to Hid.RIGHT, "⌫" to Hid.BACKSPACE, "Enter" to Hid.ENTER,
    )
    Row(
        Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal = 12.dp),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        for ((label, code) in keys) {
            AssistChip(onClick = { tap(code) }, label = { Text(label, color = Color.White) })
        }
    }
}

private fun tap(code: Int) {
    Native.nativeKey(code, true)
    Native.nativeKey(code, false)
}

/**
 * An invisible text field that opens the phone keyboard and sends what is
 * typed. It always holds one placeholder character, so a backspace on an
 * "empty" field is still seen.
 */
@Composable
private fun HiddenKeyboard() {
    val placeholder = " "
    var value by remember { mutableStateOf(TextFieldValue(placeholder, TextRange(1))) }
    val focus = remember { FocusRequester() }
    val keyboard = LocalSoftwareKeyboardController.current
    LaunchedEffect(Unit) {
        focus.requestFocus()
        keyboard?.show()
    }
    BasicTextField(
        value = value,
        onValueChange = { new ->
            val text = new.text
            when {
                text.length < placeholder.length -> tap(Hid.BACKSPACE)
                text.startsWith(placeholder) && text.length > placeholder.length ->
                    Native.nativeText(text.substring(placeholder.length))
            }
            value = TextFieldValue(placeholder, TextRange(1))
        },
        keyboardOptions = KeyboardOptions(
            keyboardType = KeyboardType.Ascii,
            imeAction = ImeAction.None,
            autoCorrectEnabled = false,
        ),
        modifier = Modifier.size(1.dp).focusRequester(focus),
    )
}
