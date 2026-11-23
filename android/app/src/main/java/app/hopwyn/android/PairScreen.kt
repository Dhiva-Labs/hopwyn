package app.hopwyn.android

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Laptop
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp

/**
 * Pairing, start to finish on one screen: the computers found on this
 * network, a way in by address when they can't be found, then the code
 * both sides compare.
 *
 * [byUser]: the user opened it, so the phone keeps its pairing window open
 * for computers to join. Otherwise a computer started pairing, and the
 * screen only shows the code.
 */
@Composable
fun PairScreen(model: HopwynModel, byUser: Boolean, onClose: () -> Unit) {
    val state = model.pairing
    val paired = remember { model.pairedCount }
    var closing by remember { mutableStateOf(false) }
    var problem by rememberSaveable { mutableStateOf<String?>(null) }
    val close = {
        closing = true
        model.cancelPairing()
        onClose()
    }
    ReleaseOnBack(close)

    LaunchedEffect(state, model.pairedCount) {
        when {
            closing -> {}
            model.pairedCount != paired -> onClose()
            state is PairingState.Failed -> {
                problem = state.why
                if (byUser) {
                    model.addComputer()
                } else {
                    model.message = state.why
                    close()
                }
            }
            state is PairingState.None -> if (byUser) model.addComputer() else onClose()
            state is PairingState.Reaching || state is PairingState.Confirm -> problem = null
            else -> {}
        }
    }

    Scaffold(
        containerColor = MaterialTheme.colorScheme.background,
        topBar = { ScreenBar("Pair a computer", close) },
    ) { padding ->
        when (state) {
            is PairingState.Confirm -> Compare(state, model, Modifier.padding(padding))
            is PairingState.Waiting -> Centered(Modifier.padding(padding)) {
                CircularProgressIndicator(Modifier.size(40.dp), strokeWidth = 3.dp)
                Spacer(Modifier.height(24.dp))
                Text("Waiting for ${state.name}", style = MaterialTheme.typography.titleLarge, textAlign = TextAlign.Center)
                Spacer(Modifier.height(8.dp))
                Text(
                    "Click Pair on ${state.name} to finish.",
                    style = MaterialTheme.typography.bodyLarge,
                    color = LocalHopwynColors.current.muted,
                    textAlign = TextAlign.Center,
                )
                Spacer(Modifier.height(32.dp))
                OutlinedButton(onClick = close) { Text("Cancel") }
            }
            else -> Find(model, state, problem, Modifier.padding(padding))
        }
    }
}

@Composable
private fun Centered(modifier: Modifier, content: @Composable () -> Unit) {
    Column(
        modifier.fillMaxSize().padding(horizontal = 28.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) { content() }
}

@Composable
private fun Compare(state: PairingState.Confirm, model: HopwynModel, modifier: Modifier) {
    Centered(modifier) {
        Text("Pair with ${state.name}?", style = MaterialTheme.typography.titleLarge, textAlign = TextAlign.Center)
        Spacer(Modifier.height(8.dp))
        Text(
            "Pair only if ${state.name} shows this same code.",
            style = MaterialTheme.typography.bodyLarge,
            color = LocalHopwynColors.current.muted,
            textAlign = TextAlign.Center,
        )
        Spacer(Modifier.height(28.dp))
        Text(
            state.code.take(3) + " " + state.code.drop(3),
            style = CodeStyle,
            color = MaterialTheme.colorScheme.onPrimaryContainer,
            modifier = Modifier
                .clip(RoundedCornerShape(20.dp))
                .background(MaterialTheme.colorScheme.primaryContainer)
                .padding(horizontal = 28.dp, vertical = 14.dp),
        )
        Spacer(Modifier.height(36.dp))
        Button(onClick = { model.confirm(true) }, modifier = Modifier.fillMaxWidth()) { Text("The codes match: pair") }
        Spacer(Modifier.height(8.dp))
        OutlinedButton(onClick = { model.confirm(false) }, modifier = Modifier.fillMaxWidth()) { Text("Cancel") }
    }
}

@Composable
private fun Find(model: HopwynModel, state: PairingState, problem: String?, modifier: Modifier) {
    val unpaired = model.nearby.filter { !it.paired }
    val muted = LocalHopwynColors.current.muted
    val focus = LocalFocusManager.current
    var address by rememberSaveable { mutableStateOf("") }
    val reaching = state is PairingState.Reaching
    val connect = {
        if (looksLikeAddress(address) && !reaching) {
            focus.clearFocus()
            model.joinAddress(address)
        }
    }

    LazyColumn(
        modifier.fillMaxSize().imePadding(),
        contentPadding = PaddingValues(start = 20.dp, end = 20.dp, bottom = 32.dp),
    ) {
        item {
            Column(Modifier.padding(top = 4.dp)) {
                Step(1, "On the computer, open Hopwyn and click Add computer.")
                Step(2, "Both show a code. If the codes match, tap Pair on both.")
            }
        }
        if (problem != null) {
            item { Notice(problem, urgent = true) }
        }
        if (Core.network != NetworkKind.Local) {
            item {
                Notice(
                    "This phone isn't on Wi-Fi. Join the Wi-Fi the computer uses, or connect the computer to this phone's hotspot.",
                )
            }
        }

        item { SectionTitle("On this network") }
        item {
            Group {
                if (unpaired.isEmpty()) {
                    Row(Modifier.fillMaxWidth().padding(16.dp), verticalAlignment = Alignment.CenterVertically) {
                        CircularProgressIndicator(Modifier.size(22.dp), strokeWidth = 2.5.dp)
                        Spacer(Modifier.width(20.dp))
                        Text("Looking for computers…", style = MaterialTheme.typography.bodyLarge, color = muted)
                    }
                }
                unpaired.forEachIndexed { i, n ->
                    if (i > 0) RowDivider()
                    ItemRow(
                        icon = Icons.Filled.Laptop,
                        title = n.name,
                        text = if (n.open) "Ready to pair" else "Click Add computer on it",
                    ) {
                        if (n.open) {
                            FilledTonalButton(onClick = { model.join(n.id) }, Modifier.padding(end = 8.dp)) { Text("Pair") }
                        }
                    }
                }
            }
        }

        item { SectionTitle("Not showing up?") }
        item {
            Group {
                Column(Modifier.padding(16.dp)) {
                    Text(
                        "Some networks hide devices from each other. Type the address the computer shows under Add computer.",
                        style = MaterialTheme.typography.bodyMedium,
                    )
                    Spacer(Modifier.height(12.dp))
                    OutlinedTextField(
                        value = address,
                        onValueChange = { v -> address = v.filter { it.isDigit() || it == '.' }.take(15) },
                        label = { Text("Computer's address") },
                        placeholder = { Text("192.168.1.20") },
                        singleLine = true,
                        enabled = !reaching,
                        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal, imeAction = ImeAction.Go),
                        keyboardActions = KeyboardActions(onGo = { connect() }),
                        modifier = Modifier.fillMaxWidth(),
                    )
                    Spacer(Modifier.height(12.dp))
                    Button(
                        onClick = connect,
                        enabled = looksLikeAddress(address) && !reaching,
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        if (reaching) {
                            CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
                            Spacer(Modifier.width(12.dp))
                            Text("Connecting…")
                        } else {
                            Text("Connect")
                        }
                    }
                }
            }
        }
    }
}
