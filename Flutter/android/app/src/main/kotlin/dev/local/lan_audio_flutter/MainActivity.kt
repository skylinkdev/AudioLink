package dev.local.lan_audio_flutter

import android.Manifest
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel
import java.io.BufferedReader
import java.io.InputStreamReader
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.InetSocketAddress
import java.net.Socket
import kotlin.math.roundToInt

class MainActivity : FlutterActivity() {
    private val channelName = "dev.local.lan_audio_flutter/audio_service"
    private var methodChannel: MethodChannel? = null
    private var playbackStoppedReceiver: BroadcastReceiver? = null

    companion object {
        private const val MIN_BUFFER_MS = 0
        private const val MIN_CUSTOM_BUFFER_MS = 1
        private const val MAX_BUFFER_MS = 2000
        private const val DEFAULT_AUTO_BUFFER_MS = 30
        private const val DEFAULT_BUFFER_MS = 120
        private const val LATENCY_PROBE_COUNT = 3
        private const val LATENCY_PROBE_TIMEOUT_MS = 800
        private const val UDP_LATENCY_PROBE_TIMEOUT_MS = 800
        private const val DEFAULT_CONTROL_PORT = 9091
        private const val CONTROL_TIMEOUT_MS = 1000
    }

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)

        val channel = MethodChannel(flutterEngine.dartExecutor.binaryMessenger, channelName)
        methodChannel = channel
        registerPlaybackStoppedReceiver()

        channel.setMethodCallHandler { call, result ->
            when (call.method) {
                "start" -> {
                    val host = call.argument<String>("host")?.trim().orEmpty()
                    val controlPort = call.argument<Int>("controlPort")
                        ?: call.argument<Int>("port")
                        ?: DEFAULT_CONTROL_PORT
                    val fallbackFrameMs = call.argument<Int>("frameMs")
                    val bufferEnabled = call.argument<Boolean>("bufferEnabled")
                        ?: call.argument<Boolean>("autoLatencyBufferEnabled")
                        ?: true
                    val hasCustomBufferMs = call.argument<Boolean>("hasCustomBufferMs") ?: false
                    val customBufferMs = call.argument<Int>("customBufferMs")
                        ?.coerceIn(MIN_CUSTOM_BUFFER_MS, MAX_BUFFER_MS)
                    val stopOnOutputDeviceChange =
                        call.argument<Boolean>("stopOnOutputDeviceChange") ?: false
                    if (host.isBlank()) {
                        result.error("invalid_host", "Host is empty", null)
                        return@setMethodCallHandler
                    }

                    Thread {
                        try {
                            val autoBuffer = bufferEnabled &&
                                !(hasCustomBufferMs && customBufferMs != null)
                            val latencyMs = 0
                            val bufferMs = when {
                                !bufferEnabled -> 0
                                hasCustomBufferMs && customBufferMs != null -> customBufferMs
                                else -> DEFAULT_AUTO_BUFFER_MS.coerceIn(MIN_BUFFER_MS, MAX_BUFFER_MS)
                            }
                            val frameMs = fetchServerFrameMs(host, controlPort) ?: fallbackFrameMs

                            runOnUiThread {
                                requestNotificationPermissionIfNeeded()
                                val intent = Intent(this, PcmAudioService::class.java).apply {
                                    action = PcmAudioService.ACTION_START
                                    putExtra(PcmAudioService.EXTRA_HOST, host)
                                    putExtra(PcmAudioService.EXTRA_CONTROL_PORT, controlPort)
                                    putExtra(PcmAudioService.EXTRA_BUFFER_MS, bufferMs)
                                    putExtra(PcmAudioService.EXTRA_LATENCY_MS, latencyMs)
                                    putExtra(PcmAudioService.EXTRA_AUTO_BUFFER, autoBuffer)
                                    putExtra(
                                        PcmAudioService.EXTRA_STOP_ON_OUTPUT_DEVICE_CHANGE,
                                        stopOnOutputDeviceChange
                                    )
                                    frameMs?.let { putExtra(PcmAudioService.EXTRA_FRAME_MS, it) }
                                }
                                ContextCompat.startForegroundService(this, intent)
                                result.success(
                                    mapOf(
                                        "latencyMs" to latencyMs,
                                        "bufferMs" to bufferMs,
                                        "frameMs" to (frameMs ?: 0),
                                    )
                                )
                            }
                        } catch (error: Exception) {
                            runOnUiThread {
                                result.error("latency_probe_failed", error.message, null)
                            }
                        }
                    }.start()
                }
                "stop" -> {
                    val intent = Intent(this, PcmAudioService::class.java).apply {
                        action = PcmAudioService.ACTION_STOP
                    }
                    startService(intent)
                    result.success(null)
                }
                "getPlaybackStats" -> {
                    result.success(PcmAudioService.playbackStats())
                }
                "clearPlaybackStats" -> {
                    PcmAudioService.clearPlaybackStats()
                    result.success(PcmAudioService.playbackStats())
                }
                "measureLatency" -> {
                    val host = call.argument<String>("host")?.trim().orEmpty()
                    val port = call.argument<Int>("port") ?: DEFAULT_CONTROL_PORT
                    if (host.isBlank()) {
                        result.error("invalid_host", "Host is empty", null)
                        return@setMethodCallHandler
                    }

                    Thread {
                        try {
                            val latencyMs = measureUdpAudioLatencyMs(host, port)
                            runOnUiThread { result.success(mapOf("latencyMs" to latencyMs)) }
                        } catch (error: Exception) {
                            runOnUiThread {
                                result.error("latency_probe_failed", error.message, null)
                            }
                        }
                    }.start()
                }
                "scanMdns" -> {
                    Thread {
                        try {
                            val devices = MdnsDiscovery(this).scan()
                                .map { device -> device.withMeasuredLatency() }
                            runOnUiThread { result.success(devices) }
                        } catch (error: Exception) {
                            runOnUiThread {
                                result.error("mdns_scan_failed", error.message, null)
                            }
                        }
                    }.start()
                }
                "computerControl" -> {
                    val host = call.argument<String>("host")?.trim().orEmpty()
                    val port = call.argument<Int>("port") ?: DEFAULT_CONTROL_PORT
                    val command = call.argument<String>("command")?.trim().orEmpty()
                    val value = call.argument<Int>("value")
                    if (host.isBlank()) {
                        result.error("invalid_host", "Host is empty", null)
                        return@setMethodCallHandler
                    }

                    Thread {
                        try {
                            val response = sendControlCommand(host, port, command, value)
                            runOnUiThread { result.success(response) }
                        } catch (error: Exception) {
                            runOnUiThread {
                                result.error("computer_control_failed", error.message, null)
                            }
                        }
                    }.start()
                }
                "queryServerStatus" -> {
                    val host = call.argument<String>("host")?.trim().orEmpty()
                    val port = call.argument<Int>("port") ?: DEFAULT_CONTROL_PORT
                    if (host.isBlank()) {
                        result.error("invalid_host", "Host is empty", null)
                        return@setMethodCallHandler
                    }

                    Thread {
                        try {
                            val response = queryServerStatus(host, port)
                            runOnUiThread { result.success(response) }
                        } catch (error: Exception) {
                            runOnUiThread {
                                result.error("server_status_failed", error.message, null)
                            }
                        }
                    }.start()
                }
                else -> result.notImplemented()
            }
        }
    }

    private fun requestNotificationPermissionIfNeeded() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            ActivityCompat.requestPermissions(this, arrayOf(Manifest.permission.POST_NOTIFICATIONS), 1001)
        }
    }

    private fun Map<String, Any?>.withMeasuredLatency(): Map<String, Any?> {
        val host = this["host"] as? String ?: return this
        val controlPort = (this["controlPort"] as? Int)
            ?: (this["port"] as? Int)
            ?: DEFAULT_CONTROL_PORT
        val latencyMs = try {
            measureUdpAudioLatencyMs(host, controlPort)
        } catch (_: Exception) {
            null
        }

        return this + mapOf("latencyMs" to latencyMs)
    }

    private fun measureConnectionLatencyMs(host: String, port: Int): Int {
        val samples = mutableListOf<Int>()

        repeat(LATENCY_PROBE_COUNT) {
            try {
                val startNs = System.nanoTime()
                Socket().use { probe ->
                    probe.tcpNoDelay = true
                    probe.soTimeout = LATENCY_PROBE_TIMEOUT_MS
                    probe.connect(InetSocketAddress(host, port), LATENCY_PROBE_TIMEOUT_MS)
                    probe.getOutputStream().write("PING\n".toByteArray(Charsets.UTF_8))
                    probe.getOutputStream().flush()
                    BufferedReader(InputStreamReader(probe.getInputStream(), Charsets.UTF_8))
                        .readLine()
                        ?: throw IllegalStateException("Empty control response")
                }
                val elapsedMs = ((System.nanoTime() - startNs) / 1_000_000.0).roundToInt()
                samples += elapsedMs.coerceAtLeast(1)
            } catch (_: Exception) {
            }
        }

        return samples.minOrNull() ?: DEFAULT_BUFFER_MS / 2
    }

    private fun measureUdpAudioLatencyMs(host: String, controlPort: Int): Int {
        val udpPort = requestUdpPingPort(host, controlPort)
        val address = InetSocketAddress(host, udpPort)
        val samples = mutableListOf<Int>()

        DatagramSocket(0).use { socket ->
            socket.soTimeout = UDP_LATENCY_PROBE_TIMEOUT_MS
            repeat(LATENCY_PROBE_COUNT) { index ->
                val payload = "LAN_AUDIO_UDP_PING_${index}_${System.nanoTime()}"
                    .toByteArray(Charsets.UTF_8)
                val packet = DatagramPacket(payload, payload.size, address)
                val responseBuffer = ByteArray(payload.size)
                val response = DatagramPacket(responseBuffer, responseBuffer.size)
                val startNs = System.nanoTime()
                socket.send(packet)
                socket.receive(response)
                val elapsedMs = ((System.nanoTime() - startNs) / 1_000_000.0).roundToInt()
                if (
                    response.length == payload.size &&
                    responseBuffer.copyOf(response.length).contentEquals(payload)
                ) {
                    samples += elapsedMs.coerceAtLeast(1)
                }
            }
        }

        return samples.minOrNull()
            ?: throw IllegalStateException("No UDP ping response")
    }

    private fun requestUdpPingPort(host: String, controlPort: Int): Int {
        Socket().use { socket ->
            socket.tcpNoDelay = true
            socket.soTimeout = CONTROL_TIMEOUT_MS
            socket.connect(InetSocketAddress(host, controlPort), CONTROL_TIMEOUT_MS)
            socket.getOutputStream().write("START_UDP_PING\n".toByteArray(Charsets.UTF_8))
            socket.getOutputStream().flush()

            val line = BufferedReader(InputStreamReader(socket.getInputStream(), Charsets.UTF_8))
                .readLine()
                ?: throw IllegalStateException("Empty UDP ping response")
            if (!line.startsWith("OK ")) {
                throw IllegalStateException(line.removePrefix("ERR ").ifBlank { line })
            }

            return parseControlFields(line)["udp_ping_port"]?.toIntOrNull()
                ?: throw IllegalStateException("Missing UDP ping port")
        }
    }

    private fun fetchServerFrameMs(host: String, port: Int): Int? {
        return try {
            Socket().use { socket ->
                socket.tcpNoDelay = true
                socket.soTimeout = CONTROL_TIMEOUT_MS
                socket.connect(InetSocketAddress(host, port), CONTROL_TIMEOUT_MS)
                socket.getOutputStream().write("PING\n".toByteArray(Charsets.UTF_8))
                socket.getOutputStream().flush()

                val line = BufferedReader(InputStreamReader(socket.getInputStream(), Charsets.UTF_8))
                    .readLine()
                    ?: return null
                if (!line.startsWith("OK ")) return null

                parseControlFields(line)["frame_ms"]?.toIntOrNull()
            }
        } catch (_: Exception) {
            null
        }
    }

    override fun cleanUpFlutterEngine(flutterEngine: FlutterEngine) {
        unregisterPlaybackStoppedReceiver()
        methodChannel?.setMethodCallHandler(null)
        methodChannel = null
        super.cleanUpFlutterEngine(flutterEngine)
    }

    private fun registerPlaybackStoppedReceiver() {
        if (playbackStoppedReceiver != null) return

        val receiver = object : BroadcastReceiver() {
            override fun onReceive(context: Context, intent: Intent) {
                if (intent.action != PcmAudioService.ACTION_PLAYBACK_STOPPED) return
                val reason = intent.getStringExtra(PcmAudioService.EXTRA_STOP_REASON).orEmpty()
                methodChannel?.invokeMethod("playbackStopped", mapOf("reason" to reason))
            }
        }
        playbackStoppedReceiver = receiver

        val filter = IntentFilter(PcmAudioService.ACTION_PLAYBACK_STOPPED)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            registerReceiver(receiver, filter, Context.RECEIVER_NOT_EXPORTED)
        } else {
            @Suppress("DEPRECATION")
            registerReceiver(receiver, filter)
        }
    }

    private fun unregisterPlaybackStoppedReceiver() {
        val receiver = playbackStoppedReceiver ?: return
        playbackStoppedReceiver = null
        try {
            unregisterReceiver(receiver)
        } catch (_: IllegalArgumentException) {
        }
    }

    private fun sendControlCommand(
        host: String,
        port: Int,
        command: String,
        value: Int?
    ): Map<String, Any> {
        val wireCommand = when (command) {
            "get" -> "GET"
            "setVolume" -> "SET_VOLUME ${value?.coerceIn(0, 100) ?: 0}"
            "setMute" -> "SET_MUTE ${if (value == 1) 1 else 0}"
            "next" -> "MEDIA_NEXT"
            "previous" -> "MEDIA_PREVIOUS"
            "playPause" -> "MEDIA_PLAY_PAUSE"
            else -> throw IllegalArgumentException("Unknown command")
        }

        Socket().use { socket ->
            socket.tcpNoDelay = true
            socket.soTimeout = CONTROL_TIMEOUT_MS
            socket.connect(InetSocketAddress(host, port), CONTROL_TIMEOUT_MS)
            socket.getOutputStream().write("$wireCommand\n".toByteArray(Charsets.UTF_8))
            socket.getOutputStream().flush()

            val line = BufferedReader(InputStreamReader(socket.getInputStream(), Charsets.UTF_8))
                .readLine()
                ?: throw IllegalStateException("Empty control response")
            if (!line.startsWith("OK ")) {
                throw IllegalStateException(line.removePrefix("ERR ").ifBlank { line })
            }

            return parseControlResponse(line)
        }
    }

    private fun parseControlResponse(line: String): Map<String, Any> {
        val fields = parseControlFields(line)

        return mapOf(
            "volume" to (fields["volume"]?.toIntOrNull() ?: 0).coerceIn(0, 100),
            "muted" to (fields["muted"] == "1"),
            "frameMs" to (fields["frame_ms"]?.toIntOrNull() ?: 0),
        )
    }

    private fun queryServerStatus(host: String, port: Int): Map<String, Any> {
        Socket().use { socket ->
            socket.tcpNoDelay = true
            socket.soTimeout = CONTROL_TIMEOUT_MS
            socket.connect(InetSocketAddress(host, port), CONTROL_TIMEOUT_MS)
            socket.getOutputStream().write("STATUS\n".toByteArray(Charsets.UTF_8))
            socket.getOutputStream().flush()

            val line = BufferedReader(InputStreamReader(socket.getInputStream(), Charsets.UTF_8))
                .readLine()
                ?: throw IllegalStateException("Empty status response")
            if (!line.startsWith("OK ")) {
                throw IllegalStateException(line.removePrefix("ERR ").ifBlank { line })
            }

            val fields = parseControlFields(line)
            return mapOf(
                "reachable" to true,
                "running" to (fields["running"] == "1"),
                "udpSession" to (fields["udp_session"] == "1"),
                "audioActive" to (fields["audio_active"] == "1"),
                "targetAddr" to fields["target_addr"].orEmpty(),
                "frameMs" to (fields["frame_ms"]?.toIntOrNull() ?: 0),
                "protocol" to fields["protocol"].orEmpty(),
            )
        }
    }

    private fun parseControlFields(line: String): Map<String, String> {
        return line.removePrefix("OK ")
            .split(' ')
            .mapNotNull {
                val parts = it.split('=', limit = 2)
                if (parts.size == 2) parts[0] to parts[1] else null
            }
            .toMap()
    }

}
