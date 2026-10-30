package dev.reaven.control

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.graphics.Color
import dev.reaven.control.data.PeerInfo
import dev.reaven.control.data.ReavenClient
import dev.reaven.control.ui.ConnectScreen
import dev.reaven.control.ui.DeviceScreen
import dev.reaven.control.ui.DevicesScreen

sealed interface AppScreen {
    data object Connect : AppScreen
    data object Devices : AppScreen
    data class Device(val peer: PeerInfo) : AppScreen
}

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            ReavenTheme {
                ReavenApp()
            }
        }
    }
}

private val ReavenColors = darkColorScheme(
    primary = Color(0xFF3FB950),
    onPrimary = Color(0xFF06210C),
    secondary = Color(0xFF58A6FF),
    background = Color(0xFF0B0D10),
    onBackground = Color(0xFFE6EDF3),
    surface = Color(0xFF14181D),
    onSurface = Color(0xFFE6EDF3),
    surfaceVariant = Color(0xFF1C2228),
    onSurfaceVariant = Color(0xFF8B98A5),
    error = Color(0xFFF85149)
)

@Composable
fun ReavenTheme(content: @Composable () -> Unit) {
    MaterialTheme(colorScheme = ReavenColors, content = content)
}

@Composable
fun ReavenApp() {
    val client = remember { ReavenClient() }
    var screen by remember { mutableStateOf<AppScreen>(AppScreen.Connect) }

    DisposableEffect(Unit) {
        onDispose { client.disconnect() }
    }

    when (val current = screen) {
        AppScreen.Connect -> ConnectScreen(
            client = client,
            onConnected = { screen = AppScreen.Devices }
        )

        AppScreen.Devices -> {
            BackHandler {
                client.disconnect()
                screen = AppScreen.Connect
            }
            DevicesScreen(
                client = client,
                onOpenDevice = { screen = AppScreen.Device(it) },
                onSignOut = {
                    client.disconnect()
                    screen = AppScreen.Connect
                }
            )
        }

        is AppScreen.Device -> {
            BackHandler { screen = AppScreen.Devices }
            DeviceScreen(
                client = client,
                peer = current.peer,
                onBack = { screen = AppScreen.Devices }
            )
        }
    }
}
