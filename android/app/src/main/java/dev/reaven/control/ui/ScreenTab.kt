package dev.reaven.control.ui

import android.graphics.BitmapFactory
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
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
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import dev.reaven.control.data.Incoming
import dev.reaven.control.data.PeerInfo
import dev.reaven.control.data.Protocol
import dev.reaven.control.data.ReavenClient
import dev.reaven.control.data.VideoSource
import dev.reaven.control.util.Gallery
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlin.math.abs
import kotlin.math.max

/**
 * Live screen view with touch-to-mouse mapping.
 *
 * Gestures:
 *   tap            -> left click
 *   press & hold   -> right click (on release)
 *   drag           -> press, move, release
 *   ▲ / ▼ buttons  -> wheel scroll
 *
 * Coordinates are normalised to the *decoded frame*, so the phone's aspect
 * ratio never has to match the desktop's.
 */
@Composable
fun ScreenTab(client: ReavenClient, peer: PeerInfo) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()

    var streaming by remember { mutableStateOf(false) }
    var bitmap by remember { mutableStateOf<ImageBitmap?>(null) }
    var frameSize by remember { mutableStateOf(IntSize.Zero) }
    var containerSize by remember { mutableStateOf(IntSize.Zero) }
    var message by remember { mutableStateOf("Not streaming") }
    var quality by remember { mutableStateOf(70) }
    var fps by remember { mutableStateOf(12) }
    var textInput by remember { mutableStateOf("") }
    var frameCount by remember { mutableStateOf(0) }

    // ---- frame pump -------------------------------------------------------
    LaunchedEffect(streaming) {
        if (!streaming) return@LaunchedEffect
        message = "Starting stream…"
        frameSize = IntSize.Zero
        bitmap = null
        frameCount = 0
        runCatching {
            client.startVideo(peer.name, VideoSource.Screen(0), quality, fps, 1600)
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
                frameSize = IntSize(frame.width, frame.height)
                frameCount += 1
                message = "${frame.width}×${frame.height} · $frameCount frames"
            }
        }
    }

    LaunchedEffect(Unit) {
        client.videoErrors.collect { err ->
            message = err
            if (err.contains("ended") || err.contains("offline")) streaming = false
        }
    }

    // ---- coordinate mapping ----------------------------------------------
    fun normalise(position: Offset): Pair<Float, Float> {
        val container = containerSize
        val frame = frameSize
        if (container.width == 0 || container.height == 0 ||
            frame.width == 0 || frame.height == 0
        ) {
            return 0.5f to 0.5f
        }
        // The image is letterboxed inside the container by ContentScale.Fit.
        val containerAspect = container.width.toFloat() / container.height
        val frameAspect = frame.width.toFloat() / frame.height
        val shownWidth: Float
        val shownHeight: Float
        if (frameAspect > containerAspect) {
            shownWidth = container.width.toFloat()
            shownHeight = shownWidth / frameAspect
        } else {
            shownHeight = container.height.toFloat()
            shownWidth = shownHeight * frameAspect
        }
        val offsetX = (container.width - shownWidth) / 2f
        val offsetY = (container.height - shownHeight) / 2f
        val nx = ((position.x - offsetX) / shownWidth).coerceIn(0f, 1f)
        val ny = ((position.y - offsetY) / shownHeight).coerceIn(0f, 1f)
        return nx to ny
    }

    fun send(events: List<org.json.JSONObject>) {
        if (events.isNotEmpty()) client.sendControl(peer.name, events)
    }

    // ---- UI --------------------------------------------------------------
    Column(Modifier.fillMaxSize()) {
        Box(
            modifier = Modifier
                .fillMaxWidth()
                .weight(1f)
                .background(Color.Black)
                .onSizeChanged { containerSize = it }
                .pointerInput(streaming, frameSize, containerSize) {
                    if (!streaming) return@pointerInput
                    awaitEachGesture {
                        val down = awaitFirstDown(requireUnconsumed = false)
                        down.consume()
                        val start = down.position
                        val downTime = System.currentTimeMillis()
                        var moved = false
                        var last = start

                        while (true) {
                            val event = awaitPointerEvent()
                            val change = event.changes.firstOrNull { it.id == down.id } ?: break
                            if (change.pressed) {
                                last = change.position
                                if ((last - start).getDistance() > 14f) {
                                    if (!moved) {
                                        moved = true
                                        val (nx, ny) = normalise(start)
                                        send(listOf(Protocol.down("left", nx, ny)))
                                    }
                                    val (nx, ny) = normalise(last)
                                    send(listOf(Protocol.move(nx, ny)))
                                }
                                change.consume()
                            } else {
                                change.consume()
                                val (nx, ny) = normalise(last)
                                val held = System.currentTimeMillis() - downTime
                                send(
                                    when {
                                        moved -> listOf(Protocol.up("left", nx, ny))
                                        held > 500 -> listOf(Protocol.click("right", nx, ny))
                                        else -> listOf(Protocol.click("left", nx, ny))
                                    }
                                )
                                break
                            }
                        }
                    }
                }
        ) {
            val current = bitmap
            if (current != null) {
                Image(
                    bitmap = current,
                    contentDescription = "Remote screen",
                    contentScale = ContentScale.Fit,
                    modifier = Modifier.fillMaxSize()
                )
            } else {
                Text(
                    text = message,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.align(Alignment.Center),
                    style = MaterialTheme.typography.bodyMedium
                )
            }
            Box(
                modifier = Modifier
                    .align(Alignment.TopStart)
                    .background(Color(0xAA000000))
                    .padding(horizontal = 8.dp, vertical = 4.dp)
            ) {
                Text(
                    text = message,
                    color = Color(0xFFB9C4CE),
                    style = MaterialTheme.typography.labelSmall,
                    fontFamily = FontFamily.Monospace
                )
            }
            // Scroll + modifier shortcuts, reachable with one thumb.
            Column(
                modifier = Modifier
                    .align(Alignment.CenterEnd)
                    .padding(8.dp),
                verticalArrangement = Arrangement.spacedBy(6.dp)
            ) {
                RoundKey("▲") { send(listOf(Protocol.scroll(0.5f, 0.5f, -2))) }
                RoundKey("▼") { send(listOf(Protocol.scroll(0.5f, 0.5f, 2))) }
            }
        }

        // ---- control bar --------------------------------------------------
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .background(MaterialTheme.colorScheme.surface)
                .padding(horizontal = 12.dp, vertical = 8.dp)
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Button(
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
                    enabled = streaming,
                    onClick = {
                        send(listOf(Protocol.scroll(0.5f, 0.5f, 2)))
                    }
                ) { Text("Scroll") }

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
            }

            Spacer(Modifier.height(6.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                SmallKey("Enter") { send(Protocol.tapKey("Return")) }
                SmallKey("Esc") { send(Protocol.tapKey("escape")) }
                SmallKey("Tab") { send(Protocol.tapKey("tab")) }
                SmallKey("Ctrl+C") { send(listOf(Protocol.combo("ctrl", "c"))) }
                SmallKey("Alt+Tab") { send(listOf(Protocol.combo("alt", "tab"))) }
                SmallKey("Win") { send(listOf(Protocol.combo("meta"))) }
            }

            Spacer(Modifier.height(6.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                OutlinedTextField(
                    value = textInput,
                    onValueChange = { textInput = it },
                    placeholder = { Text("Type text on the remote machine") },
                    singleLine = true,
                    modifier = Modifier.weight(1f),
                    keyboardOptions = KeyboardOptions(imeAction = ImeAction.Send),
                    keyboardActions = KeyboardActions(
                        onSend = {
                            if (textInput.isNotEmpty()) {
                                send(listOf(Protocol.text(textInput)))
                                textInput = ""
                            }
                        }
                    )
                )
                Spacer(Modifier.width(8.dp))
                Button(
                    enabled = streaming,
                    onClick = {
                        if (textInput.isNotEmpty()) {
                            send(listOf(Protocol.text(textInput)))
                            textInput = ""
                        }
                    }
                ) { Text("Send") }
            }

            Spacer(Modifier.height(4.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    "Quality $quality · ${fps}fps",
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant
                )
                Spacer(Modifier.weight(1f))
                SmallKey("q-5") { quality = max(20, quality - 5) }
                SmallKey("q+5") { quality = (quality + 5).coerceAtMost(95) }
                SmallKey("f-") { fps = max(2, fps - 2) }
                SmallKey("f+") { fps = (fps + 2).coerceAtMost(30) }
            }
        }
    }
}

@Composable
private fun RoundKey(label: String, onClick: () -> Unit) {
    OutlinedButton(
        onClick = onClick,
        modifier = Modifier.size(46.dp),
        contentPadding = androidx.compose.foundation.layout.PaddingValues(0.dp)
    ) { Text(label, style = MaterialTheme.typography.labelLarge) }
}

@Composable
private fun SmallKey(label: String, onClick: () -> Unit) {
    OutlinedButton(
        onClick = onClick,
        contentPadding = androidx.compose.foundation.layout.PaddingValues(
            horizontal = 8.dp,
            vertical = 2.dp
        )
    ) { Text(label, style = MaterialTheme.typography.labelSmall, maxLines = 1) }
}
