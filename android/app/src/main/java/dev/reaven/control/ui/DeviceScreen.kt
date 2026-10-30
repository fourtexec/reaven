package dev.reaven.control.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Tab
import androidx.compose.material3.TabRow
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import dev.reaven.control.data.PeerInfo
import dev.reaven.control.data.ReavenClient

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DeviceScreen(
    client: ReavenClient,
    peer: PeerInfo,
    onBack: () -> Unit
) {
    var tab by remember { mutableStateOf(0) }
    val titles = listOf("Screen", "Console", "Camera")

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Column {
                        Text(
                            text = peer.name,
                            style = MaterialTheme.typography.titleMedium,
                            fontFamily = FontFamily.Monospace
                        )
                        Text(
                            text = listOf(peer.hostname, peer.os)
                                .filter { it.isNotBlank() }
                                .joinToString(" · "),
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant
                        )
                    }
                },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                    }
                },
                colors = TopAppBarDefaults.topAppBarColors(
                    containerColor = MaterialTheme.colorScheme.surface
                )
            )
        }
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
        ) {
            TabRow(
                selectedTabIndex = tab,
                containerColor = MaterialTheme.colorScheme.surface
            ) {
                titles.forEachIndexed { index, title ->
                    Tab(
                        selected = tab == index,
                        onClick = { tab = index },
                        text = { Text(title) }
                    )
                }
            }
            when (tab) {
                0 -> ScreenTab(client, peer)
                1 -> ConsoleTab(client, peer)
                else -> CameraTab(client, peer)
            }
        }
    }
}
