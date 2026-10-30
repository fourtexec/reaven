package dev.reaven.control.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import dev.reaven.control.data.PeerInfo
import dev.reaven.control.data.ReavenClient
import kotlinx.coroutines.launch

/**
 * Command console: a one-shot `exec` round-trip per entered command.
 *
 * This is deliberately not a raw PTY view — ANSI escapes are painful to render
 * faithfully on a phone, and command-by-command output is what you actually
 * want from a remote console. For a full PTY, use `reaven cl` on a desktop.
 */
@Composable
fun ConsoleTab(client: ReavenClient, peer: PeerInfo) {
    val scope = rememberCoroutineScope()
    var command by remember { mutableStateOf("") }
    var history by remember { mutableStateOf<List<Pair<String, String>>>(emptyList()) }
    var busy by remember { mutableStateOf(false) }
    var scrollState = rememberScrollState()

    fun run(cmd: String) {
        val trimmed = cmd.trim()
        if (trimmed.isEmpty() || busy) return
        busy = true
        scope.launch {
            val output = runCatching { client.exec(peer.name, trimmed) }
                .getOrElse { "error: ${it.message}" }
            history = history + (("$ $trimmed") to output.trimEnd())
            busy = false
            command = ""
        }
    }

    LaunchedEffect(history.size) {
        scrollState.animateScrollTo(scrollState.maxValue)
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(12.dp)
    ) {
        Column(
            modifier = Modifier
                .weight(1f)
                .fillMaxWidth()
                .background(Color(0xFF0E1116))
                .padding(10.dp)
                .verticalScroll(scrollState)
        ) {
            if (history.isEmpty()) {
                Text(
                    text = "Commands run in a bash login shell on ${peer.name}.",
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    style = MaterialTheme.typography.bodySmall,
                    fontFamily = FontFamily.Monospace
                )
            }
            history.forEach { (cmd, output) ->
                Text(
                    text = cmd,
                    color = Color(0xFF3FB950),
                    style = MaterialTheme.typography.bodySmall,
                    fontFamily = FontFamily.Monospace
                )
                Text(
                    text = if (output.isBlank()) "(no output)" else output,
                    color = Color(0xFFE6EDF3),
                    style = MaterialTheme.typography.bodySmall,
                    fontFamily = FontFamily.Monospace,
                    modifier = Modifier.horizontalScroll(rememberScrollState())
                )
                Spacer(Modifier.height(10.dp))
            }
        }

        Spacer(Modifier.height(8.dp))
        Row(
            modifier = Modifier.horizontalScroll(rememberScrollState()),
            horizontalArrangement = Arrangement.spacedBy(6.dp)
        ) {
            listOf(
                "uname -a",
                "uptime",
                "df -h",
                "ps aux --sort=-%cpu | head",
                "nproc"
            ).forEach { preset ->
                OutlinedButton(
                    onClick = { run(preset) },
                    contentPadding = androidx.compose.foundation.layout.PaddingValues(
                        horizontal = 8.dp,
                        vertical = 2.dp
                    )
                ) { Text(preset, style = MaterialTheme.typography.labelSmall, maxLines = 1) }
            }
        }

        Spacer(Modifier.height(8.dp))
        Row(verticalAlignment = Alignment.CenterVertically) {
            OutlinedTextField(
                value = command,
                onValueChange = { command = it },
                placeholder = { Text("command…") },
                singleLine = true,
                textStyle = MaterialTheme.typography.bodyMedium.copy(
                    fontFamily = FontFamily.Monospace
                ),
                modifier = Modifier.weight(1f),
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Send),
                keyboardActions = KeyboardActions(onSend = { run(command) })
            )
            Spacer(Modifier.width(8.dp))
            Button(onClick = { run(command) }, enabled = !busy) {
                if (busy) {
                    CircularProgressIndicator(
                        modifier = Modifier.width(16.dp).height(16.dp),
                        strokeWidth = 2.dp,
                        color = MaterialTheme.colorScheme.onPrimary
                    )
                } else {
                    Text("Run")
                }
            }
        }
        Spacer(Modifier.height(4.dp))
        Row {
            OutlinedButton(
                onClick = { history = emptyList() },
                contentPadding = androidx.compose.foundation.layout.PaddingValues(
                    horizontal = 8.dp,
                    vertical = 2.dp
                )
            ) { Text("Clear", style = MaterialTheme.typography.labelSmall) }
        }
    }
}
