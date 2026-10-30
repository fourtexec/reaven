package dev.reaven.control.data

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.channels.BufferOverflow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout
import org.json.JSONArray
import org.json.JSONObject
import java.io.BufferedWriter
import java.io.IOException
import java.io.OutputStreamWriter
import java.net.InetSocketAddress
import java.net.Socket
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap

/**
 * A single operator connection to a Reaven relay.
 *
 * One socket carries everything: request/response traffic, video frames and
 * shell output. Frames are dispatched to the right place by their `type` tag,
 * and request responses are matched by id on [CompletableDeferred]s.
 */
class ReavenClient {

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    private var socket: Socket? = null
    private var writer: BufferedWriter? = null
    private var readerJob: Job? = null
    private var welcome: CompletableDeferred<Result<Unit>>? = null

    private val pendingRequests = ConcurrentHashMap<String, CompletableDeferred<Incoming.Response>>()
    private val pendingPeers = ConcurrentHashMap<String, CompletableDeferred<List<PeerInfo>>>()

    private val _status = MutableStateFlow("not connected")
    val status: StateFlow<String> = _status.asStateFlow()

    private val _videoFrames = MutableSharedFlow<Incoming.VideoFrame>(
        extraBufferCapacity = 8,
        onBufferOverflow = BufferOverflow.DROP_OLDEST
    )
    val videoFrames: SharedFlow<Incoming.VideoFrame> = _videoFrames.asSharedFlow()

    private val _videoErrors = MutableSharedFlow<String>(extraBufferCapacity = 8)
    val videoErrors: SharedFlow<String> = _videoErrors.asSharedFlow()

    private val _shellOutput = MutableSharedFlow<ByteArray>(
        extraBufferCapacity = 128,
        onBufferOverflow = BufferOverflow.DROP_OLDEST
    )
    val shellOutput: SharedFlow<ByteArray> = _shellOutput.asSharedFlow()

    private val _shellExit = MutableSharedFlow<Int>(extraBufferCapacity = 2)
    val shellExit: SharedFlow<Int> = _shellExit.asSharedFlow()

    var currentStreamId: String? = null
        private set

    private var shellId: String? = null
    private var shellTarget: String? = null

    // ------------------------------------------------------------------ setup

    suspend fun connect(
        host: String,
        port: Int,
        password: String?,
        name: String
    ): Result<Unit> = withContext(Dispatchers.IO) {
        runCatching {
            disconnect()
            val s = Socket()
            s.tcpNoDelay = true
            s.connect(InetSocketAddress(host, port), 8_000)
            socket = s
            writer = BufferedWriter(OutputStreamWriter(s.getOutputStream(), Charsets.UTF_8))
            val signal = CompletableDeferred<Result<Unit>>()
            welcome = signal
            readerJob = scope.launch { readLoop(s, signal) }
            sendFrame(Protocol.hello(name, password))
            withTimeout(10_000) { signal.await() }.getOrThrow()
            _status.value = "connected to $host:$port"
        }.onFailure {
            _status.value = "connect failed: ${it.message}"
            closeQuietly()
        }
    }

    private suspend fun readLoop(s: Socket, signal: CompletableDeferred<Result<Unit>>) {
        try {
            val reader = s.getInputStream().bufferedReader(Charsets.UTF_8)
            while (true) {
                val line = reader.readLine() ?: break
                if (line.isBlank()) continue
                when (val frame = Protocol.parse(line)) {
                    null -> Unit
                    is Incoming.Welcome -> signal.complete(Result.success(Unit))
                    is Incoming.ServerError -> {
                        signal.complete(Result.failure(IOException(frame.message)))
                        _status.value = "server error: ${frame.message}"
                    }
                    is Incoming.Response -> pendingRequests.remove(frame.id)?.complete(frame)
                    is Incoming.PeerList -> pendingPeers.remove(frame.id)?.complete(frame.peers)
                    is Incoming.VideoFrame -> {
                        if (frame.id == currentStreamId) _videoFrames.tryEmit(frame)
                    }
                    is Incoming.VideoError -> _videoErrors.tryEmit(frame.message)
                    is Incoming.VideoStop -> _videoErrors.tryEmit("stream ended")
                    is Incoming.ShellOutput -> _shellOutput.tryEmit(frame.data)
                    is Incoming.ShellExit -> _shellExit.tryEmit(frame.code)
                }
            }
        } catch (e: Exception) {
            if (e !is java.io.InterruptedIOException) {
                _status.value = "disconnected: ${e.message}"
            }
        } finally {
            signal.complete(Result.failure(IOException("connection closed")))
            _status.value = "disconnected"
        }
    }

    private fun sendFrame(json: JSONObject) {
        val out = writer ?: throw IOException("not connected")
        synchronized(this) {
            out.write(json.toString())
            out.write("\n")
            out.flush()
        }
    }

    fun closeQuietly() {
        runCatching { readerJob?.cancel() }
        val id = shellId
        val target = shellTarget
        if (id != null && target != null) {
            runCatching { sendFrame(Protocol.shellClose(id, target)) }
        }
        runCatching { socket?.close() }
        socket = null
        writer = null
        currentStreamId = null
        shellId = null
    }

    fun disconnect() = closeQuietly()

    // ------------------------------------------------------------- discovery

    suspend fun listPeers(): List<PeerInfo> {
        val id = newId()
        val deferred = CompletableDeferred<List<PeerInfo>>()
        pendingPeers[id] = deferred
        try {
            sendFrame(Protocol.listPeers(id))
            return withTimeout(8_000) { deferred.await() }
        } finally {
            pendingPeers.remove(id)
        }
    }

    // ------------------------------------------------------------- one-shots

    suspend fun request(target: String, op: JSONObject, timeoutMs: Long = 25_000): JSONObject {
        val id = newId()
        val deferred = CompletableDeferred<Incoming.Response>()
        pendingRequests[id] = deferred
        try {
            sendFrame(Protocol.request(id, target, op))
            val response = withTimeout(timeoutMs) { deferred.await() }
            response.error?.let { throw IOException(it) }
            return response.payload ?: JSONObject()
        } finally {
            pendingRequests.remove(id)
        }
    }

    suspend fun screenshot(target: String, monitor: Int = 0, quality: Int = 85): ByteArray {
        val payload = request(target, Protocol.opScreenshot(monitor, quality, 0))
        return payload.imageBytes()
    }

    suspend fun cameraSnapshot(target: String, index: Int = 0, quality: Int = 85): ByteArray {
        val payload = request(target, Protocol.opCameraSnapshot(index, 0, quality))
        return payload.imageBytes()
    }

    suspend fun ping(target: String): String =
        request(target, Protocol.opPing()).textOut()

    suspend fun exec(target: String, command: String): String {
        val payload = request(target, Protocol.opExec(command))
        return payload.textOut() + payload.textErr()
    }

    data class CameraEntry(val index: Int, val name: String)

    suspend fun listCameras(target: String): List<CameraEntry> {
        val payload = request(target, Protocol.opListCameras())
        val array = JSONArray(payload.optString("data"))
        return (0 until array.length()).mapNotNull { i ->
            val item = array.optJSONObject(i) ?: return@mapNotNull null
            CameraEntry(item.optInt("index"), item.optString("name"))
        }
    }

    suspend fun listScreens(target: String): List<String> {
        val payload = request(target, Protocol.opListScreens())
        val array = JSONArray(payload.optString("data"))
        return (0 until array.length()).mapNotNull { i ->
            val item = array.optJSONObject(i) ?: return@mapNotNull null
            val index = item.optInt("index")
            val w = item.optInt("width")
            val h = item.optInt("height")
            "$index: ${item.optString("name")} (${w}x$h)"
        }
    }

    // ----------------------------------------------------------------- video

    fun startVideo(
        target: String,
        source: VideoSource,
        quality: Int,
        fps: Int,
        maxWidth: Int
    ): String {
        val id = newId()
        currentStreamId = id
        sendFrame(Protocol.videoStart(id, target, source, maxWidth, quality, fps))
        return id
    }

    fun stopVideo(target: String) {
        val id = currentStreamId ?: return
        runCatching { sendFrame(Protocol.videoStop(id, target)) }
        currentStreamId = null
    }

    /** Fire-and-forget: input latency matters more than a per-event ack. */
    fun sendControl(target: String, events: List<JSONObject>) {
        if (events.isEmpty()) return
        runCatching {
            sendFrame(Protocol.control(newId(), target, currentStreamId, events))
        }
    }

    // ----------------------------------------------------------------- shell

    fun openShell(target: String, rows: Int = 40, cols: Int = 100): String {
        val id = newId()
        shellId = id
        shellTarget = target
        sendFrame(Protocol.shellStart(id, target, rows, cols))
        return id
    }

    fun shellInput(bytes: ByteArray) {
        val id = shellId ?: return
        val target = shellTarget ?: return
        runCatching { sendFrame(Protocol.shellInput(id, target, bytes)) }
    }

    fun typeShell(text: String) = shellInput(text.toByteArray(Charsets.UTF_8))

    fun resizeShell(rows: Int, cols: Int) {
        val id = shellId ?: return
        val target = shellTarget ?: return
        runCatching { sendFrame(Protocol.shellResize(id, target, rows, cols)) }
    }

    fun closeShell() {
        val id = shellId ?: return
        val target = shellTarget ?: return
        runCatching { sendFrame(Protocol.shellClose(id, target)) }
        shellId = null
    }

    /** Waits for the first frame of the current stream (used before a snapshot). */
    suspend fun awaitFirstFrame(timeoutMs: Long = 6_000): Incoming.VideoFrame =
        withTimeout(timeoutMs) { videoFrames.first() }

    private fun newId(): String = UUID.randomUUID().toString()
}
