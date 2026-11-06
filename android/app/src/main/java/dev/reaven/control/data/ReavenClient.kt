package dev.reaven.control.data

import android.annotation.SuppressLint
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.channels.BufferOverflow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import org.json.JSONArray
import org.json.JSONObject
import java.io.IOException
import java.security.SecureRandom
import java.security.cert.X509Certificate
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.TimeUnit
import javax.net.ssl.SSLContext
import javax.net.ssl.TrustManager
import javax.net.ssl.X509TrustManager

/**
 * A single operator connection to a Reaven relay over secure WebSocket (WSS).
 *
 * One connection carries everything: request/response traffic, video frames and
 * shell output. Every WebSocket text message is one JSON frame, dispatched by
 * its `type` tag; request responses are matched by id on [CompletableDeferred]s.
 */
class ReavenClient {

    private val pendingRequests = ConcurrentHashMap<String, CompletableDeferred<Incoming.Response>>()
    private val pendingPeers = ConcurrentHashMap<String, CompletableDeferred<List<PeerInfo>>>()

    @Volatile
    private var webSocket: WebSocket? = null

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
        name: String,
        insecure: Boolean = false
    ): Result<Unit> = withContext(Dispatchers.IO) {
        runCatching {
            disconnect()
            val signal = CompletableDeferred<Result<Unit>>()
            val client = if (insecure) insecureHttpClient() else baseHttpClient
            val request = Request.Builder()
                .url("wss://$host:$port/ws")
                .build()
            val socket = client.newWebSocket(request, object : WebSocketListener() {
                override fun onOpen(webSocket: WebSocket, response: Response) {
                    runCatching {
                        webSocket.send(encodeFrame(Protocol.hello(name, password)))
                    }.onFailure {
                        signal.complete(Result.failure(IOException(it.message ?: "send failed")))
                    }
                }

                override fun onMessage(webSocket: WebSocket, text: String) {
                    handleFrame(text, signal)
                }

                override fun onClosing(webSocket: WebSocket, code: Int, reason: String) {
                    webSocket.close(1000, null)
                }

                override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
                    signal.complete(Result.failure(IOException("connection closed")))
                    _status.value = "disconnected"
                }

                override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
                    signal.complete(
                        Result.failure(IOException(t.message ?: "connection failed"))
                    )
                    _status.value = "connect failed: ${t.message}"
                }
            })
            webSocket = socket
            withTimeout(10_000) { signal.await() }.getOrThrow()
            _status.value = "connected to $host:$port (WSS)"
        }.onFailure {
            _status.value = "connect failed: ${it.message}"
            closeQuietly()
        }
    }

    private fun handleFrame(text: String, signal: CompletableDeferred<Result<Unit>>) {
        when (val frame = Protocol.parse(text)) {
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

    private fun encodeFrame(json: JSONObject): String = json.toString()

    private fun sendFrame(json: JSONObject) {
        val socket = webSocket ?: throw IOException("not connected")
        synchronized(this) {
            if (!socket.send(encodeFrame(json))) {
                throw IOException("connection closed")
            }
        }
    }

    fun closeQuietly() {
        val id = shellId
        val target = shellTarget
        if (id != null && target != null) {
            runCatching { sendFrame(Protocol.shellClose(id, target)) }
        }
        runCatching { webSocket?.close(1000, "bye") }
        webSocket = null
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

    private companion object {
        val baseHttpClient: OkHttpClient by lazy {
            OkHttpClient.Builder()
                .connectTimeout(8, TimeUnit.SECONDS)
                // No read timeout: a screen stream is intentionally long-lived.
                .readTimeout(0, TimeUnit.MILLISECONDS)
                .pingInterval(20, TimeUnit.SECONDS)
                .build()
        }
    }

    @SuppressLint("CustomX509TrustManager", "TrustAllX509TrustManager")
    private fun insecureHttpClient(): OkHttpClient {
        val trustManager = object : X509TrustManager {
            override fun checkClientTrusted(chain: Array<out X509Certificate>?, authType: String?) {}
            override fun checkServerTrusted(chain: Array<out X509Certificate>?, authType: String?) {}
            override fun getAcceptedIssuers(): Array<X509Certificate> = emptyArray()
        }
        val sslContext = SSLContext.getInstance("TLS")
        sslContext.init(null, arrayOf<TrustManager>(trustManager), SecureRandom())
        return baseHttpClient.newBuilder()
            .sslSocketFactory(sslContext.socketFactory, trustManager)
            .hostnameVerifier { _, _ -> true }
            .build()
    }
}
