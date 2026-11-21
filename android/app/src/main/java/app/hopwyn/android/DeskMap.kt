package app.hopwyn.android

import android.provider.Settings
import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Laptop
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import kotlin.math.cos
import kotlin.math.roundToInt
import kotlin.math.sin

private val PhoneW = 46.dp
private val PhoneH = 82.dp
private val NodeW = 120.dp
private val NodeH = 84.dp

/**
 * This phone in the middle, linked to each paired computer: a solid line
 * while connected, dashed while not. Tapping a connected computer opens the
 * trackpad for it; with none paired, an empty slot starts pairing.
 */
@Composable
fun DeskMap(
    computers: List<Computer>,
    onUse: (Computer) -> Unit,
    onPair: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val scheme = MaterialTheme.colorScheme
    val extra = LocalHopwynColors.current
    val context = LocalContext.current
    val still = remember {
        Settings.Global.getFloat(context.contentResolver, Settings.Global.ANIMATOR_DURATION_SCALE, 1f) == 0f
    }
    // One slow pulse travelling out along live links: the only ambient motion.
    val pulse by rememberInfiniteTransition(label = "link").animateFloat(
        initialValue = 0f,
        targetValue = 1f,
        animationSpec = infiniteRepeatable(tween(2200, easing = LinearEasing), RepeatMode.Restart),
        label = "pulse",
    )

    // Room above and below the phone only once computers sit there too.
    val mapHeight = when {
        computers.size >= 4 -> 300.dp
        computers.size == 3 -> 236.dp
        else -> 160.dp
    }
    // One computer: centre the pair, not the phone, so the link has room.
    val shift = if (computers.size <= 1) 74.dp else 0.dp
    BoxWithConstraints(modifier.fillMaxWidth().height(mapHeight)) {
        val density = LocalDensity.current
        val w = with(density) { maxWidth.toPx() }
        val h = with(density) { mapHeight.toPx() }
        val shiftPx = with(density) { shift.toPx() }
        // With three, nothing sits below: the phone moves down, under the top one.
        val centerY = if (computers.size == 3) h - with(density) { (PhoneH / 2 + 26.dp).toPx() } else h / 2
        val center = Offset(w / 2 + shiftPx, centerY)
        val slots = if (computers.size <= 1) {
            listOf(Offset(w / 2 - shiftPx, h / 2))
        } else {
            slots(computers.size, w, h, density.density).map { o ->
                if (computers.size == 3) {
                    if (o.y < h / 2 - 1) Offset(o.x, with(density) { (NodeH / 2).toPx() }) else Offset(o.x, centerY)
                } else {
                    o
                }
            }
        }

        Canvas(Modifier.fillMaxSize()) {
            val shown = if (computers.isEmpty()) listOf(null) else computers
            shown.forEachIndexed { i, c ->
                val end = slots[i]
                val online = c?.online == true
                drawLine(
                    color = if (online) extra.online else extra.line,
                    start = center,
                    end = end,
                    strokeWidth = 3.dp.toPx(),
                    cap = StrokeCap.Round,
                    pathEffect = if (online) null else PathEffect.dashPathEffect(floatArrayOf(10f, 12f)),
                )
                if (online && !still) {
                    val t = pulse
                    drawCircle(
                        color = extra.online,
                        radius = 5.dp.toPx(),
                        center = center + (end - center) * t,
                        alpha = 1f - t * 0.6f,
                    )
                }
            }
            // This phone.
            val pw = PhoneW.toPx()
            val ph = PhoneH.toPx()
            val topLeft = Offset(center.x - pw / 2, center.y - ph / 2)
            drawRoundRect(scheme.surfaceVariant, topLeft, Size(pw, ph), CornerRadius(10.dp.toPx()))
            drawRoundRect(
                scheme.primary, topLeft, Size(pw, ph), CornerRadius(10.dp.toPx()),
                style = Stroke(2.5.dp.toPx()),
            )
            drawRoundRect(
                scheme.primary,
                Offset(center.x - 7.dp.toPx(), topLeft.y + 6.dp.toPx()),
                Size(14.dp.toPx(), 3.dp.toPx()),
                CornerRadius(2.dp.toPx()),
            )
        }
        Text(
            "This phone",
            style = MaterialTheme.typography.labelMedium,
            color = extra.muted,
            modifier = Modifier.align(Alignment.TopCenter).offset(
                x = shift,
                y = with(density) { center.y.toDp() } + PhoneH / 2 + 4.dp,
            ),
        )

        if (computers.isEmpty()) {
            EmptySlot(slots[0], onPair)
        } else {
            computers.forEachIndexed { i, c -> ComputerNode(c, slots[i], onUse) }
        }
    }
}

/** Where each computer sits around the phone, as centre points in px. */
private fun slots(n: Int, w: Float, h: Float, density: Float): List<Offset> {
    val c = Offset(w / 2, h / 2)
    val rx = (w / 2 - NodeW.value * density / 2 - 4 * density).coerceAtLeast(0f)
    val ry = h / 2 - NodeH.value * density / 2
    // Left first, then right, then above and below: like screens on a desk.
    val angles = when (n) {
        1 -> listOf(180.0)
        2 -> listOf(180.0, 0.0)
        3 -> listOf(180.0, 0.0, 270.0)
        4 -> listOf(180.0, 0.0, 270.0, 90.0)
        else -> List(n) { 180.0 + 360.0 * it / n }
    }
    return angles.map { a ->
        val r = Math.toRadians(a)
        Offset(c.x + (rx * cos(r)).toFloat(), c.y + (ry * sin(r)).toFloat())
    }
}

@Composable
private fun ComputerNode(c: Computer, at: Offset, onUse: (Computer) -> Unit) {
    val scheme = MaterialTheme.colorScheme
    val extra = LocalHopwynColors.current
    NodeBox(at, NodeW, NodeH) {
        Column(
            Modifier
                .fillMaxSize()
                .clip(RoundedCornerShape(16.dp))
                .background(scheme.surfaceVariant)
                .border(
                    1.5.dp,
                    if (c.online) extra.online else extra.line,
                    RoundedCornerShape(16.dp),
                )
                .clickable(enabled = c.online, role = Role.Button) { onUse(c) }
                .semantics {
                    contentDescription = if (c.online) "Use ${c.name}" else "${c.name}, not connected"
                }
                .padding(horizontal = 8.dp, vertical = 8.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.Center,
        ) {
            Box {
                Icon(
                    Icons.Filled.Laptop, null,
                    Modifier.size(28.dp),
                    tint = if (c.online) scheme.onSurface else extra.muted,
                )
                Box(
                    Modifier
                        .align(Alignment.TopEnd)
                        .offset(x = 4.dp, y = (-2).dp)
                        .size(9.dp)
                        .clip(CircleShape)
                        .background(if (c.online) extra.online else extra.line),
                )
            }
            Text(
                c.name,
                style = MaterialTheme.typography.labelMedium,
                color = if (c.online) scheme.onSurface else extra.muted,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
                textAlign = TextAlign.Center,
                modifier = Modifier.width(NodeW - 16.dp).padding(top = 4.dp),
            )
        }
    }
}

@Composable
private fun EmptySlot(at: Offset, onPair: () -> Unit) {
    val extra = LocalHopwynColors.current
    val scheme = MaterialTheme.colorScheme
    NodeBox(at, NodeW, NodeH) {
        Box(
            Modifier
                .fillMaxSize()
                .clip(RoundedCornerShape(16.dp))
                .clickable(role = Role.Button, onClick = onPair)
                .semantics { contentDescription = "Pair a computer" },
            contentAlignment = Alignment.Center,
        ) {
            Canvas(Modifier.fillMaxSize()) {
                drawRoundRect(
                    extra.line,
                    cornerRadius = CornerRadius(16.dp.toPx()),
                    style = Stroke(1.5.dp.toPx(), pathEffect = PathEffect.dashPathEffect(floatArrayOf(10f, 10f))),
                )
            }
            Icon(Icons.Filled.Add, null, tint = scheme.primary)
        }
    }
}

/** A [w]×[h] box centred on [at]. */
@Composable
private fun NodeBox(at: Offset, w: Dp, h: Dp, content: @Composable () -> Unit) {
    val density = LocalDensity.current
    val x = with(density) { (at.x - w.toPx() / 2).roundToInt() }
    val y = with(density) { (at.y - h.toPx() / 2).roundToInt() }
    Box(Modifier.offset { IntOffset(x, y) }.size(w, h)) { content() }
}
