package dev.reaven.control.ui

import android.content.Context
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import dev.reaven.control.data.ReavenClient
import kotlinx.coroutines.launch

@Composable
fun ConnectScreen(
    client: ReavenClient,
    onConnected: () -> Unit
) {
    val context = LocalContext.current
    val prefs = remember { context.getSharedPreferences("reaven", Context.MODE_PRIVATE) }
    val scope = rememberCoroutineScope()

    var host by remember { mutableStateOf(prefs.getString("host", "") ?: "") }
    var port by remember { mutableStateOf(prefs.getString("port", "7331") ?: "7331") }
    var password by remember { mutableStateOf(prefs.getString("password", "") ?: "") }
    var deviceName by remember {
        mutableStateOf(prefs.getString("deviceName", "android") ?: "android")
    }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }

    fun submit() {
        val parsedPort = port.trim().toIntOrNull()
        if (host.isBlank()) {
            error = "Enter the relay host or IP"
            return
        }
        if (parsedPort == null || parsedPort !in 1..65535) {
            error = "Port must be between 1 and 65535"
            return
        }
        busy = true
        error = null
        scope.launch {
            val result = client.connect(
                host = host.trim(),
                port = parsedPort,
                password = password.ifBlank { null },
                name = deviceName.ifBlank { "android" }
            )
            busy = false
            result.fold(
                onSuccess = {
                    prefs.edit()
                        .putString("host", host.trim())
                        .putString("port", parsedPort.toString())
                        .putString("password", password)
                        .putString("deviceName", deviceName)
                        .apply()
                    onConnected()
                },
                onFailure = { error = it.message ?: "connection failed" }
            )
        }
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(24.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally
    ) {
        Text(
            text = "REAVEN",
            style = MaterialTheme.typography.headlineLarge,
            color = MaterialTheme.colorScheme.primary,
            fontFamily = FontFamily.Monospace
        )
        Text(
            text = "remote control console",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant
        )
        Spacer(Modifier.height(28.dp))

        OutlinedTextField(
            value = host,
            onValueChange = { host = it },
            label = { Text("Relay host or IP") },
            placeholder = { Text("192.168.1.20") },
            singleLine = true,
            enabled = !busy,
            keyboardOptions = KeyboardOptions(imeAction = ImeAction.Next),
            modifier = Modifier.fillMaxWidth()
        )
        Spacer(Modifier.height(12.dp))
        OutlinedTextField(
            value = port,
            onValueChange = { port = it.filter(Char::isDigit) },
            label = { Text("Port") },
            singleLine = true,
            enabled = !busy,
            keyboardOptions = KeyboardOptions(
                keyboardType = KeyboardType.Number,
                imeAction = ImeAction.Next
            ),
            modifier = Modifier.fillMaxWidth()
        )
        Spacer(Modifier.height(12.dp))
        OutlinedTextField(
            value = password,
            onValueChange = { password = it },
            label = { Text("Password") },
            singleLine = true,
            enabled = !busy,
            visualTransformation = PasswordVisualTransformation(),
            keyboardOptions = KeyboardOptions(
                keyboardType = KeyboardType.Password,
                imeAction = ImeAction.Next
            ),
            modifier = Modifier.fillMaxWidth()
        )
        Spacer(Modifier.height(12.dp))
        OutlinedTextField(
            value = deviceName,
            onValueChange = { deviceName = it },
            label = { Text("This device's name") },
            singleLine = true,
            enabled = !busy,
            keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done),
            modifier = Modifier.fillMaxWidth()
        )

        Spacer(Modifier.height(20.dp))
        Button(
            onClick = ::submit,
            enabled = !busy,
            modifier = Modifier.fillMaxWidth()
        ) {
            if (busy) {
                CircularProgressIndicator(
                    modifier = Modifier.size(18.dp),
                    strokeWidth = 2.dp,
                    color = MaterialTheme.colorScheme.onPrimary
                )
                Spacer(Modifier.size(10.dp))
                Text("Connecting…")
            } else {
                Text("Connect")
            }
        }

        error?.let {
            Spacer(Modifier.height(14.dp))
            Text(
                text = it,
                color = MaterialTheme.colorScheme.error,
                style = MaterialTheme.typography.bodySmall
            )
        }

        Spacer(Modifier.height(28.dp))
        Text(
            text = "Start the relay with:\n  reaven server --listen 0.0.0.0:7331 --password …\n" +
                "and each machine with:\n  reaven connect --server <this-pc>:7331 --name desktop",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            fontFamily = FontFamily.Monospace
        )
    }
}
