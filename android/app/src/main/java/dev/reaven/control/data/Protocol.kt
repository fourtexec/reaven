package dev.reaven.control.data

import android.util.Base64
import org.json.JSONArray
import org.json.JSONObject

/**
 * Wire protocol for Reaven: newline-delimited JSON, one frame per line.
 *
 * This mirrors `src/protocol.rs` exactly. Frames are tagged with a `type`
 * field; payloads are tagged with `op` (requests) or `kind` (results).
 */
object Protocol {

    // ---------------------------------------------------------------- builders

    fun hello(name: String, password: String?): JSONObject = JSONObject()
        .put("type", "hello")
        .put("name", name)
        .put("password", password ?: JSONObject.NULL)
        .put("kind", "operator")
        .put("info", JSONObject.NULL)

    fun listPeers(id: String): JSONObject = JSONObject()
        .put("type", "list_peers")
        .put("id", id)
        .put("from", "")

    fun request(id: String, target: String, op: JSONObject): JSONObject = JSONObject()
        .put("type", "request")
        .put("id", id)
        .put("from", "")
        .put("target", target)
        .put("op", op)

    // ------------------------------------------------------------- operations

    fun opExec(command: String): JSONObject =
        JSONObject().put("op", "exec").put("command", command)

    fun opScreenshot(monitor: Int = 0, quality: Int = 85, maxWidth: Int = 0): JSONObject =
        JSONObject()
            .put("op", "screenshot")
            .put("monitor", monitor)
            .put("quality", quality)
            .put("max_width", maxWidth)

    fun opCameraSnapshot(index: Int = 0, width: Int = 0, quality: Int = 85): JSONObject =
        JSONObject()
            .put("op", "camera_snapshot")
            .put("index", index)
            .put("width", width)
            .put("quality", quality)

    fun opListScreens(): JSONObject = JSONObject().put("op", "list_screens")
    fun opListCameras(): JSONObject = JSONObject().put("op", "list_cameras")
    fun opPing(): JSONObject = JSONObject().put("op", "ping")

    fun opDownload(path: String): JSONObject =
        JSONObject().put("op", "download").put("path", path)

    fun opUpload(path: String, dataBase64: String): JSONObject =
        JSONObject().put("op", "upload").put("path", path).put("data", dataBase64)

    // ----------------------------------------------------------------- video

    fun videoStart(
        id: String,
        target: String,
        source: VideoSource,
        maxWidth: Int,
        quality: Int,
        fps: Int
    ): JSONObject = JSONObject()
        .put("type", "video_start")
        .put("id", id)
        .put("from", "")
        .put("target", target)
        .put("source", source.toJson())
        .put("max_width", maxWidth)
        .put("quality", quality)
        .put("fps", fps)

    fun videoStop(id: String, to: String): JSONObject = JSONObject()
        .put("type", "video_stop")
        .put("id", id)
        .put("from", "")
        .put("to", to)

    fun control(
        id: String,
        target: String,
        streamId: String?,
        events: List<JSONObject>
    ): JSONObject = JSONObject()
        .put("type", "control")
        .put("id", id)
        .put("from", "")
        .put("target", target)
        .put("stream", streamId ?: JSONObject.NULL)
        .put("events", JSONArray(events))

    // ----------------------------------------------------------------- shell

    fun shellStart(id: String, target: String, rows: Int, cols: Int): JSONObject = JSONObject()
        .put("type", "shell_start")
        .put("id", id)
        .put("from", "")
        .put("target", target)
        .put("rows", rows)
        .put("cols", cols)
        .put("term", "xterm-256color")

    fun shellInput(id: String, to: String, bytes: ByteArray): JSONObject = JSONObject()
        .put("type", "shell_input")
        .put("id", id)
        .put("from", "")
        .put("to", to)
        .put("data", b64(bytes))

    fun shellResize(id: String, to: String, rows: Int, cols: Int): JSONObject = JSONObject()
        .put("type", "shell_resize")
        .put("id", id)
        .put("from", "")
        .put("to", to)
        .put("rows", rows)
        .put("cols", cols)

    fun shellClose(id: String, to: String): JSONObject = JSONObject()
        .put("type", "shell_close")
        .put("id", id)
        .put("from", "")
        .put("to", to)

    // ---------------------------------------------------------- input events

    fun move(x: Float, y: Float): JSONObject =
        JSONObject().put("event", "move").put("x", x.toDouble()).put("y", y.toDouble())

    fun down(button: String, x: Float, y: Float): JSONObject = JSONObject()
        .put("event", "down").put("button", button)
        .put("x", x.toDouble()).put("y", y.toDouble())

    fun up(button: String, x: Float, y: Float): JSONObject = JSONObject()
        .put("event", "up").put("button", button)
        .put("x", x.toDouble()).put("y", y.toDouble())

    fun click(button: String, x: Float, y: Float): JSONObject = JSONObject()
        .put("event", "click").put("button", button)
        .put("x", x.toDouble()).put("y", y.toDouble())

    fun doubleClick(button: String, x: Float, y: Float): JSONObject = JSONObject()
        .put("event", "double_click").put("button", button)
        .put("x", x.toDouble()).put("y", y.toDouble())

    fun scroll(x: Float, y: Float, dy: Int, dx: Int = 0): JSONObject = JSONObject()
        .put("event", "scroll")
        .put("x", x.toDouble()).put("y", y.toDouble())
        .put("dx", dx).put("dy", dy)

    fun key(name: String, pressed: Boolean): JSONObject =
        JSONObject().put("event", "key").put("key", name).put("pressed", pressed)

    fun tapKey(name: String): List<JSONObject> = listOf(key(name, true), key(name, false))

    fun text(value: String): JSONObject =
        JSONObject().put("event", "text").put("text", value)

    fun combo(vararg keys: String): JSONObject =
        JSONObject().put("event", "combo").put("keys", JSONArray(keys.toList()))

    // ----------------------------------------------------------------- helpers

    fun b64(bytes: ByteArray): String = Base64.encodeToString(bytes, Base64.NO_WRAP)

    fun unb64(value: String): ByteArray =
        Base64.decode(value, Base64.DEFAULT)

    fun parse(line: String): Incoming? {
        val json = runCatching { JSONObject(line) }.getOrNull() ?: return null
        return when (json.optString("type")) {
            "welcome" -> Incoming.Welcome
            "error" -> Incoming.ServerError(json.optString("message"))
            "peer_list" -> Incoming.PeerList(
                id = json.optString("id"),
                peers = json.optJSONArray("peers").toPeerList()
            )
            "response" -> {
                val payload = json.optJSONObject("result")
                Incoming.Response(
                    id = json.optString("id"),
                    payload = payload,
                    error = if (payload == null) json.optString("result").ifBlank { "request failed" } else null
                )
            }
            "video_frame" -> Incoming.VideoFrame(
                id = json.optString("id"),
                seq = json.optLong("seq"),
                width = json.optInt("width"),
                height = json.optInt("height"),
                jpeg = unb64(json.optString("data"))
            )
            "video_error" -> Incoming.VideoError(
                id = json.optString("id"),
                message = json.optString("message")
            )
            "video_stop" -> Incoming.VideoStop(json.optString("id"))
            "shell_output" -> Incoming.ShellOutput(
                id = json.optString("id"),
                data = unb64(json.optString("data"))
            )
            "shell_exit" -> Incoming.ShellExit(
                id = json.optString("id"),
                code = json.optInt("code")
            )
            else -> null
        }
    }

    /** Decode the `json` payload kind (monitor/camera lists). */
    fun jsonArrayOf(payload: JSONObject): JSONArray =
        JSONArray(payload.optString("data"))

    private fun JSONArray?.toPeerList(): List<PeerInfo> {
        val out = mutableListOf<PeerInfo>()
        if (this == null) return out
        for (i in 0 until length()) {
            val item = optJSONObject(i) ?: continue
            out += PeerInfo(
                name = item.optString("name"),
                hostname = item.optString("hostname"),
                os = item.optString("os"),
                version = item.optString("version")
            )
        }
        return out
    }
}

data class PeerInfo(
    val name: String,
    val hostname: String = "",
    val os: String = "",
    val version: String = ""
)

sealed class VideoSource(val kind: String) {
    data class Screen(val monitor: Int = 0) : VideoSource("screen") {
        override fun toJson(): JSONObject =
            JSONObject().put("kind", "screen").put("monitor", monitor)
    }

    data class Camera(val index: Int = 0) : VideoSource("camera") {
        override fun toJson(): JSONObject =
            JSONObject().put("kind", "camera").put("index", index)
    }

    abstract fun toJson(): JSONObject
}

sealed class Incoming {
    data object Welcome : Incoming()
    data class ServerError(val message: String) : Incoming()
    data class PeerList(val id: String, val peers: List<PeerInfo>) : Incoming()
    data class Response(val id: String, val payload: JSONObject?, val error: String?) : Incoming()
    data class VideoFrame(
        val id: String,
        val seq: Long,
        val width: Int,
        val height: Int,
        val jpeg: ByteArray
    ) : Incoming()

    data class VideoError(val id: String, val message: String) : Incoming()
    data class VideoStop(val id: String) : Incoming()
    data class ShellOutput(val id: String, val data: ByteArray) : Incoming()
    data class ShellExit(val id: String, val code: Int) : Incoming()
}

/** Convenience accessors for the `kind`-tagged result payloads. */
val JSONObject.payloadKind: String get() = optString("kind")

fun JSONObject.textOut(): String = optString("stdout")
fun JSONObject.textErr(): String = optString("stderr")
fun JSONObject.textCode(): Int = optInt("code", 0)

fun JSONObject.imageBytes(): ByteArray = Protocol.unb64(optString("data"))
