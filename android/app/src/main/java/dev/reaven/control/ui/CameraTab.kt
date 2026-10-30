package dev.reaven.control.ui

import android.graphics.BitmapFactory
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import dev.reaven.control.data.PeerInfo
import dev.reaven.control.data.ReavenClient
import dev.reaven.control.data.VideoSource
import dev.reaven.control.util.Gallery
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * Webcam view for a remote machine.
 *
 * Note that no input mapping is offered here: a camera frame has no desktop
 * geometry to map pointer coordinates onto.
 */
@Composable
fun CameraTab(client: ReavenClient, peer: PeerInfo) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()

    var cameras by remember { mutableStateOf<List<ReavenClient.CameraEntry>>(emptyList()) }
    var selected by remember { mutableStateOf(0) }
    var streaming by remember { mutableStateOf(false) }
    var bitmap by remember { mutableStateOf<ImageBitmap?>(null) }
    var message by remember { mutableStateOf("Detecting cameras…") }
    var loading by remember { mutableStateOf(true) }

    LaunchedEffect(Unit) {
        loading = true
        runCatching { client.listCameras(peer.name) }
            .onSuccess {
                cameras = it
                message = if (it.isEmpty()) "No webcams found on ${peer.name}" else "Ready"
            }
            .onFailure { message = "Could not list cameras: ${it.message}" }
        loading = false
    }

    LaunchedEffect(streaming, selected) {
        if (!streaming) return@LaunchedEffect
        message = "Starting camera $selected…"
        runCatching {
            client.startVideo(peer.name, VideoSource.Camera(selected), 75, 10, 1280)
        }.onFailure {
            message = "Could not start: ${it.message}"
            streaming = false
        }
    }

    DisposableEffect(Unit) {
        onDispose { client.stopVideo(peer.name) }
    }

    LaunchedEffect(Unit) {
        client.videoFrames.collect { frame ->
            val decoded = withContext(Dispatchers.Default) {
                BitmapFactory.decodeByteArray(frame.jpeg, 0, frame.jpeg.size)?.asImageBitmap()
            }
            if (decoded != null) {
                bitmap = decoded
                message = "Camera $selected · ${frame.width}×${frame.height}"
            }
        }
    }

    LaunchedEffect(Unit) {
        client.videoErrors.collect { err ->
            message = err
            if (err.contains("ended")) streaming = false
        }
    }

    Column(Modifier.fillMaxSize()) {
        Box(
            modifier = Modifier
                .fillMaxWidth()
                .weight(1f)
                .background(Color.Black),
            contentAlignment = Alignment.Center
        ) {
            val current = bitmap
            if (current != null) {
                Image(
                    bitmap = current,
                    contentDescription = "Remote webcam",
                    contentScale = ContentScale.Fit,
                    modifier = Modifier.fillMaxSize()
                )
            } else if (loading) {
                CircularProgressIndicator(color = MaterialTheme.colorScheme.primary)
            } else {
                Text(
                    text = message,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    fontFamily = FontFamily.Monospace,
                    style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.padding(24.dp)
                )
            }
        }

        Column(
            modifier = Modifier
                .fillMaxWidth()
                .background(MaterialTheme.colorScheme.surface)
                .padding(12.dp)
        ) {
            if (cameras.isNotEmpty()) {
                LazyRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    items(cameras, key = { it.index }) { cam ->
                        FilterChip(
                            selected = cam.index == selected,
                            onClick = { selected = cam.index },
                            label = {
                                Text(
                                    cam.name.take(24),
                                    style = MaterialTheme.typography.labelSmall
                                )
                            }
                        )
                    }
                }
                Spacer(Modifier.height(8.dp))
            }
            Row(verticalAlignment = Alignment.CenterVertically) {
                Button(
                    enabled = cameras.isNotEmpty(),
                    onClick = {
                        if (streaming) {
                            client.stopVideo(peer.name)
                            streaming = false
                            message = "Stopped"
                        } else {
                            streaming = true
                        }
                    },
                    colors = ButtonDefaults.buttonColors(
                        containerColor = if (streaming) MaterialTheme.colorScheme.error
                        else MaterialTheme.colorScheme.primary
                    )
                ) { Text(if (streaming) "Stop" else "Start") }

                Spacer(Modifier.width(8.dp))
                OutlinedButton(
                    enabled = bitmap != null,
                    onClick = {
                        val current = bitmap
                        if (current != null) {
                            scope.launch {
                                runCatching { Gallery.save(context, current) }
                                    .onSuccess { message = "Saved to gallery" }
                                    .onFailure { message = "Save failed: ${it.message}" }
                            }
                        }
                    }
                ) { Text("Snapshot") }

                Spacer(Modifier.width(8.dp))
                OutlinedButton(
                    enabled = cameras.isNotEmpty(),
                    onClick = {
                        scope.launch {
                            message = "Capturing…"
                            runCatching { client.cameraSnapshot(peer.name, selected) }
                                .onSuccess { jpeg ->
                                    val decoded = withContext(Dispatchers.Default) {
                                        BitmapFactory.decodeByteArray(jpeg, 0, jpeg.size)
                                    }
                                    if (decoded != null) {
                                        Gallery.save(context, decoded.asImageBitmap())
                                        message = "Single frame saved"
                                    }
                                }
                                .onFailure { message = "Capture failed: ${it.message}" }
                        }
                    }
                ) { Text("One frame") }
            }
            Spacer(Modifier.height(6.dp))
            Text(
                text = message,
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                fontFamily = FontFamily.Monospace
            )
        }
    }
}
