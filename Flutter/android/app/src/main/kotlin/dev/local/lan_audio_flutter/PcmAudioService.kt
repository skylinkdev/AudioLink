package dev.local.lan_audio_flutter

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.media.AudioAttributes
import android.media.AudioDeviceCallback
import android.media.AudioDeviceInfo
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioTrack
import android.os.Build
import android.os.IBinder
import android.os.Process
import android.util.Log
import androidx.core.app.NotificationCompat
import java.io.BufferedReader
import java.io.InputStreamReader
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.InetSocketAddress
import java.net.Socket
import java.net.SocketTimeoutException
import java.util.HashSet
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong
import kotlin.concurrent.thread
import kotlin.math.max
import kotlin.math.roundToInt

class PcmAudioService : Service() {
    companion object {
        const val ACTION_START = "dev.local.lan_audio_flutter.START"
        const val ACTION_STOP = "dev.local.lan_audio_flutter.STOP"
        const val ACTION_PLAYBACK_STOPPED = "dev.local.lan_audio_flutter.PLAYBACK_STOPPED"
        const val EXTRA_STOP_REASON = "reason"
        const val STOP_REASON_USER = "user"
        const val STOP_REASON_UDP_TIMEOUT = "udp_timeout"
        const val STOP_REASON_OUTPUT_DEVICE_CHANGED = "output_device_changed"
        const val STOP_REASON_SERVICE_DESTROYED = "service_destroyed"
        const val EXTRA_HOST = "host"
        const val EXTRA_CONTROL_PORT = "controlPort"
        const val EXTRA_BUFFER_MS = "bufferMs"
        const val EXTRA_LATENCY_MS = "latencyMs"
        const val EXTRA_FRAME_MS = "frameMs"
        const val EXTRA_AUTO_BUFFER = "autoBuffer"
        const val EXTRA_STOP_ON_OUTPUT_DEVICE_CHANGE = "stopOnOutputDeviceChange"

        private const val CHANNEL_ID = "lan_audio_playback"
        private const val NOTIFICATION_ID = 42
        private const val LOG_TAG = "PcmAudioService"

        // PCM sample rate sent by the Rust service.
        private const val SAMPLE_RATE = 48000

        // Stereo PCM.
        private const val CHANNELS = 2

        // 16-bit PCM.
        private const val BYTES_PER_SAMPLE = 2
        private const val PCM_FRAME_BYTES = CHANNELS * BYTES_PER_SAMPLE

        // Fallback for manual connections or old mDNS records that do not publish frame_ms.
        private const val DEFAULT_FRAME_MS = 10

        private const val MIN_BUFFER_MS = 0
        private const val MAX_BUFFER_MS = 2000
        private const val MIN_FRAME_MS = 1
        private const val MAX_FRAME_MS = 1000
        private const val MIN_AUTO_BUFFER_MS = 30
        private const val AUTO_BUFFER_INTERVAL_WINDOW_SIZE = 512
        private const val AUTO_BUFFER_AVG_MULTIPLIER = 3L
        private const val AUTO_BUFFER_P95_MULTIPLIER = 2L
        private const val AUTO_BUFFER_DECREASE_STABLE_MS = 5000L
        private const val MAX_JITTER_BUFFER_EXTRA_MS = 250
        private const val AUDIO_TRACK_BUFFER_FRAME_MULTIPLIER = 2
        private const val MIN_PACKET_TIMEOUT_MS = 10L
        private const val MAX_PACKET_TIMEOUT_MS = 250L
        private const val PACKET_TIMEOUT_FRAME_MULTIPLIER = 2L
        private const val PACKET_TIMEOUT_LOW_WATERMARK_DIVISOR = 4
        private const val PLAYBACK_STRETCH_90_PERCENT_RATIO = 1.0
        private const val PLAYBACK_STRETCH_80_PERCENT_RATIO = 1.001
        private const val PLAYBACK_STRETCH_70_PERCENT_RATIO = 1.002
        private const val PLAYBACK_STRETCH_60_PERCENT_RATIO = 1.003
        private const val PLAYBACK_STRETCH_50_PERCENT_RATIO = 1.004
        private const val PLAYBACK_STRETCH_UNDER_50_PERCENT_RATIO = 1.005
        private const val STRETCH_LOG_INTERVAL_MS = 1000L

        private const val CONNECT_TIMEOUT_MS = 2000
        private const val UDP_TIMEOUT_MS = 3000
        private const val UDP_RECEIVE_BUFFER_BYTES = 1024 * 1024
        private const val RECONNECT_DELAY_MS = 1000L
        private const val CONTROL_TIMEOUT_MS = 1000
        private const val REAL_STREAM_WARMUP_MS = 1000L

        private val UDP_MAGIC = byteArrayOf(
            'L'.code.toByte(),
            'A'.code.toByte(),
            'P'.code.toByte(),
            '1'.code.toByte()
        )
        private const val UDP_VERSION = 1
        private const val UDP_HEADER_BYTES = 48
        private const val UDP_PACKET_FEC_CODEC_NONE = 0
        private const val UDP_PACKET_FEC_CODEC_PCM_S16LE_STEREO = 1
        private const val DUPLICATE_PACKET_HISTORY_MULTIPLIER = 4

        private val statsActive = AtomicBoolean(false)
        private val statsReceivedPackets = AtomicLong(0)
        private val statsLostPackets = AtomicLong(0)
        private val statsTimedOutPackets = AtomicLong(0)
        private val statsDiscardedPackets = AtomicLong(0)
        private val statsReleasedAudioTrackUnderruns = AtomicLong(0)
        private val statsAudioTrackUnderrunBaseline = AtomicLong(0)
        private val statsLastReceivedAtMs = AtomicLong(0)
        private val statsReceiveIntervalTotalMs = AtomicLong(0)
        private val statsReceiveIntervalCount = AtomicLong(0)
        private val statsMaxReceiveIntervalMs = AtomicLong(0)
        private val statsFrameMs = AtomicLong(0)
        private val statsBufferedFrames = AtomicLong(0)
        private val statsOrderedBufferedFrames = AtomicLong(0)
        private val statsPeakBufferedFrames = AtomicLong(0)
        private val statsTargetBufferFrames = AtomicLong(0)
        private val statsPlaybackStretchRatioPermille = AtomicLong(1000)

        @Volatile
        private var statsAudioTrack: AudioTrack? = null

        @Volatile
        private var statsHost = ""

        @Volatile
        private var statsControlPort = 0

        @Volatile
        private var statsStartedAtMs = 0L

        fun playbackStats(): Map<String, Any> {
            return mapOf(
                "isRunning" to statsActive.get(),
                "host" to statsHost,
                "controlPort" to statsControlPort,
                "startedAtMs" to statsStartedAtMs,
                "receivedPackets" to statsReceivedPackets.get(),
                "lostPackets" to statsLostPackets.get(),
                "timedOutPackets" to statsTimedOutPackets.get(),
                "discardedPackets" to statsDiscardedPackets.get(),
                "audioTrackUnderruns" to totalAudioTrackUnderruns(),
                "averageReceiveIntervalMs" to averageReceiveIntervalMs(),
                "maxReceiveIntervalMs" to statsMaxReceiveIntervalMs.get(),
                "frameMs" to statsFrameMs.get(),
                "bufferedFrames" to statsBufferedFrames.get(),
                "orderedBufferedFrames" to statsOrderedBufferedFrames.get(),
                "targetBufferFrames" to statsTargetBufferFrames.get(),
                "bufferedMs" to framesToMs(statsBufferedFrames.get(), statsFrameMs.get()),
                "orderedBufferedMs" to framesToMs(
                    statsOrderedBufferedFrames.get(),
                    statsFrameMs.get()
                ),
                "targetBufferMs" to framesToMs(
                    statsTargetBufferFrames.get(),
                    statsFrameMs.get()
                ),
                "playbackStretchRatio" to statsPlaybackStretchRatioPermille.get() / 1000.0,
            )
        }

        fun clearPlaybackStats() {
            statsReceivedPackets.set(0)
            statsLostPackets.set(0)
            statsTimedOutPackets.set(0)
            statsDiscardedPackets.set(0)
            statsReleasedAudioTrackUnderruns.set(0)
            statsAudioTrackUnderrunBaseline.set(readAudioTrackUnderruns(statsAudioTrack))
            statsLastReceivedAtMs.set(0)
            statsReceiveIntervalTotalMs.set(0)
            statsReceiveIntervalCount.set(0)
            statsMaxReceiveIntervalMs.set(0)
            statsPeakBufferedFrames.set(statsBufferedFrames.get())
            statsStartedAtMs = if (statsActive.get()) System.currentTimeMillis() else 0L
        }

        private fun resetPlaybackStats(host: String, controlPort: Int) {
            statsReceivedPackets.set(0)
            statsLostPackets.set(0)
            statsTimedOutPackets.set(0)
            statsDiscardedPackets.set(0)
            statsReleasedAudioTrackUnderruns.set(0)
            statsAudioTrackUnderrunBaseline.set(0)
            statsLastReceivedAtMs.set(0)
            statsReceiveIntervalTotalMs.set(0)
            statsReceiveIntervalCount.set(0)
            statsMaxReceiveIntervalMs.set(0)
            statsFrameMs.set(0)
            statsBufferedFrames.set(0)
            statsOrderedBufferedFrames.set(0)
            statsPeakBufferedFrames.set(0)
            statsTargetBufferFrames.set(0)
            statsPlaybackStretchRatioPermille.set(1000)
            statsAudioTrack = null
            statsHost = host
            statsControlPort = controlPort
            statsStartedAtMs = System.currentTimeMillis()
            statsActive.set(true)
        }

        private fun markPlaybackInactive() {
            statsActive.set(false)
        }

        private fun recordReceivedPacket() {
            statsReceivedPackets.incrementAndGet()
            val nowMs = System.currentTimeMillis()
            val previousMs = statsLastReceivedAtMs.getAndSet(nowMs)
            if (previousMs > 0 && nowMs >= previousMs) {
                val intervalMs = nowMs - previousMs
                statsReceiveIntervalTotalMs.addAndGet(intervalMs)
                statsReceiveIntervalCount.incrementAndGet()
                statsMaxReceiveIntervalMs.updateAndGet { current -> max(current, intervalMs) }
            }
        }

        private fun averageReceiveIntervalMs(): Long {
            val count = statsReceiveIntervalCount.get()
            if (count <= 0) return 0
            return statsReceiveIntervalTotalMs.get() / count
        }

        private fun recordLostPackets(count: Long) {
            if (count > 0) statsLostPackets.addAndGet(count)
        }

        private fun recordTimedOutPackets(count: Long) {
            if (count > 0) statsTimedOutPackets.addAndGet(count)
        }

        private fun recordDiscardedPacket() {
            statsDiscardedPackets.incrementAndGet()
        }

        private fun setStatsBufferConfig(
            frameMs: Int,
            targetBufferFrames: Int
        ) {
            statsFrameMs.set(frameMs.toLong())
            setStatsTargetBufferFrames(targetBufferFrames)
            recordBufferedFrames(0)
        }

        private fun setStatsTargetBufferFrames(targetBufferFrames: Int) {
            statsTargetBufferFrames.set(targetBufferFrames.toLong().coerceAtLeast(0))
        }

        private fun recordPlaybackStretchRatio(stretchRatio: Double) {
            statsPlaybackStretchRatioPermille.set((stretchRatio * 1000).roundToInt().toLong())
        }

        private fun recordBufferedFrames(bufferedFrames: Int) {
            val frames = bufferedFrames.toLong().coerceAtLeast(0)
            statsBufferedFrames.set(frames)
            statsPeakBufferedFrames.updateAndGet { current -> max(current, frames) }
        }

        private fun recordOrderedBufferedFrames(orderedBufferedFrames: Int) {
            statsOrderedBufferedFrames.set(orderedBufferedFrames.toLong().coerceAtLeast(0))
        }

        private fun setStatsAudioTrack(track: AudioTrack) {
            statsAudioTrack = track
            statsAudioTrackUnderrunBaseline.set(0)
        }

        private fun recordClosedStatsAudioTrack(track: AudioTrack?) {
            if (track == null || statsAudioTrack !== track) return
            val underruns = readAudioTrackUnderruns(track) -
                statsAudioTrackUnderrunBaseline.get()
            if (underruns > 0) statsReleasedAudioTrackUnderruns.addAndGet(underruns)
            statsAudioTrackUnderrunBaseline.set(0)
            statsAudioTrack = null
        }

        private fun totalAudioTrackUnderruns(): Long {
            val currentUnderruns = readAudioTrackUnderruns(statsAudioTrack) -
                statsAudioTrackUnderrunBaseline.get()
            return statsReleasedAudioTrackUnderruns.get() + currentUnderruns.coerceAtLeast(0)
        }

        private fun readAudioTrackUnderruns(track: AudioTrack?): Long {
            if (track == null || Build.VERSION.SDK_INT < Build.VERSION_CODES.N) return 0
            return try {
                track.underrunCount.toLong()
            } catch (_: Exception) {
                0
            }
        }

        private fun framesToMs(frames: Long, frameMs: Long): Long {
            if (frames <= 0 || frameMs <= 0) return 0
            return frames * frameMs
        }
    }

    private val running = AtomicBoolean(false)
    private var playbackThread: Thread? = null
    private var udpSocket: DatagramSocket? = null
    private var audioTrack: AudioTrack? = null
    private var outputDeviceCallback: AudioDeviceCallback? = null
    private var initialOutputDeviceSignature: Set<String>? = null
    private var playbackStoppedNotified = false

    override fun onCreate() {
        super.onCreate()
        createNotificationChannel()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP -> {
                stopPlayback()
                notifyPlaybackStopped(STOP_REASON_USER)
                stopForeground(STOP_FOREGROUND_REMOVE)
                stopSelf()
                return START_NOT_STICKY
            }
            ACTION_START -> {
                playbackStoppedNotified = false
                val host = intent.getStringExtra(EXTRA_HOST).orEmpty()
                val controlPort = intent.getIntExtra(EXTRA_CONTROL_PORT, 9091)
                val bufferMs = intent.getIntExtra(EXTRA_BUFFER_MS, MIN_BUFFER_MS)
                    .coerceIn(MIN_BUFFER_MS, MAX_BUFFER_MS)
                val latencyMs = intent.getIntExtra(EXTRA_LATENCY_MS, bufferMs / 2)
                val frameMs = intent.getIntExtra(EXTRA_FRAME_MS, DEFAULT_FRAME_MS)
                    .coerceIn(MIN_FRAME_MS, MAX_FRAME_MS)
                val autoBuffer = intent.getBooleanExtra(EXTRA_AUTO_BUFFER, false)
                val stopOnOutputDeviceChange =
                    intent.getBooleanExtra(EXTRA_STOP_ON_OUTPUT_DEVICE_CHANGE, false)
                startForeground(NOTIFICATION_ID, buildNotification("Connecting to $host:$controlPort"))
                startPlayback(
                    host,
                    controlPort,
                    bufferMs,
                    latencyMs,
                    frameMs,
                    autoBuffer,
                    stopOnOutputDeviceChange
                )
            }
        }

        return START_STICKY
    }

    override fun onDestroy() {
        stopPlayback()
        notifyPlaybackStopped(STOP_REASON_SERVICE_DESTROYED)
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    private fun startPlayback(
        host: String,
        controlPort: Int,
        bufferMs: Int,
        latencyMs: Int,
        frameMs: Int,
        autoBuffer: Boolean,
        stopOnOutputDeviceChange: Boolean
    ) {
        stopPlayback()
        resetPlaybackStats(host, controlPort)
        running.set(true)
        registerOutputDeviceChangeStopIfNeeded(stopOnOutputDeviceChange)

        playbackThread = thread(name = "pcm-audio-playback") {
            while (running.get()) {
                try {
                    playOneConnection(
                        host,
                        controlPort,
                        bufferMs,
                        latencyMs,
                        frameMs,
                        autoBuffer
                    )
                } catch (error: Exception) {
                    if (running.get()) {
                        if (error is SocketTimeoutException) {
                            running.set(false)
                            markPlaybackInactive()
                            updateNotification("Stopped: no UDP audio received from $host:$controlPort")
                            notifyPlaybackStopped(STOP_REASON_UDP_TIMEOUT)
                            stopForeground(STOP_FOREGROUND_REMOVE)
                            stopSelf()
                        } else {
                            val message = error.message ?: error.javaClass.simpleName
                            updateNotification(
                                "Reconnecting to $host:$controlPort in ${RECONNECT_DELAY_MS}ms ($message)"
                            )
                        }
                    }
                } finally {
                    runCatching { sendControlCommand(host, controlPort, "STOP_UDP") }
                    closePlaybackResources()
                }

                if (running.get()) {
                    sleepBeforeReconnect(RECONNECT_DELAY_MS)
                }
            }
        }
    }

    private fun playOneConnection(
        host: String,
        controlPort: Int,
        bufferMs: Int,
        latencyMs: Int,
        frameMs: Int,
        autoBuffer: Boolean
    ) {
        updateNotification("Connecting to $host:$controlPort")

        val audioTrackBufferBytes = bytesForDuration(
            frameMs * AUDIO_TRACK_BUFFER_FRAME_MULTIPLIER
        )

        updateNotification("Warming playback $host:$controlPort")
        openPlaybackResources(audioTrackBufferBytes).use { warmup ->
            playSilentStreamWarmup(host, controlPort, warmup, frameMs)
        }

        updateNotification("Restarting playback $host:$controlPort")
        openPlaybackResources(audioTrackBufferBytes).use { playback ->
            playStartedStream(
                host = host,
                controlPort = controlPort,
                bufferMs = bufferMs,
                latencyMs = latencyMs,
                frameMs = frameMs,
                autoBuffer = autoBuffer,
                resources = playback
            )
        }
    }

    private fun playStartedStream(
        host: String,
        controlPort: Int,
        bufferMs: Int,
        latencyMs: Int,
        frameMs: Int,
        autoBuffer: Boolean,
        resources: PlaybackResources
    ) {
        sendControlCommand(host, controlPort, "START_UDP ${resources.receiver.localPort}")

        val maxPayloadBytes = bytesForDuration(frameMs)
        val maxFecBytes = fecBytesForDuration(frameMs)
        val maxPacketBytes = UDP_HEADER_BYTES + maxPayloadBytes + maxFecBytes
        val targetBufferFrames = AtomicInteger(framesForDuration(bufferMs, frameMs))
        val jitterBufferCapacityMs = if (autoBuffer) MAX_BUFFER_MS else bufferMs
        val maxBufferedFrames = framesForDuration(
            jitterBufferCapacityMs + jitterBufferOverflowHeadroomMs(jitterBufferCapacityMs),
            frameMs
        )
        setStatsBufferConfig(frameMs, targetBufferFrames.get())
        recordPlaybackStretchRatio(1.0)
        Log.i(
            LOG_TAG,
            "playback buffer target=${targetBufferFrames.get()}frames/${bufferMs}ms stretch=auto"
        )
        val jitterBuffer = JitterBuffer(maxBufferedFrames, maxPayloadBytes, maxFecBytes)
        val duplicateFilter = DuplicatePacketFilter(
            maxBufferedFrames * DUPLICATE_PACKET_HISTORY_MULTIPLIER
        )
        val autoBufferController = AutoBufferController(frameMs, targetBufferFrames.get())
        fun updateAutoBufferTarget() {
            if (!autoBuffer) return
            val updatedTargetFrames = autoBufferController.targetFrames() ?: return
            val previousTargetFrames = targetBufferFrames.getAndSet(updatedTargetFrames)
            if (previousTargetFrames == updatedTargetFrames) return

            setStatsTargetBufferFrames(updatedTargetFrames)
            Log.i(
                LOG_TAG,
                "auto playback buffer target=${updatedTargetFrames}frames/" +
                    "${updatedTargetFrames * frameMs}ms"
            )
        }

        val receiverRunning = AtomicBoolean(true)
        val receiverError = AtomicReference<Exception?>(null)
        val receiverThread = thread(name = "pcm-audio-receiver") {
            setCurrentUdpReceiverThreadRealtimePriority()
            val packetBuffer = ByteArray(maxPacketBytes)
            val datagram = DatagramPacket(packetBuffer, packetBuffer.size)
            var lastAcceptedPacketAtMs = 0L
            while (running.get() && receiverRunning.get()) {
                try {
                    val frame = receivePcmPayload(resources.receiver, datagram) ?: continue
                    if (duplicateFilter.isDuplicate(frame.sequence)) {
                        continue
                    }
                    val receivedAtMs = System.currentTimeMillis()
                    if (autoBuffer && lastAcceptedPacketAtMs > 0 && receivedAtMs >= lastAcceptedPacketAtMs) {
                        autoBufferController.recordInterval(receivedAtMs - lastAcceptedPacketAtMs)
                    }
                    lastAcceptedPacketAtMs = receivedAtMs
                    recordReceivedPacket()
                    updateAutoBufferTarget()
                    if (jitterBuffer.put(frame, packetBuffer)) {
                        recordBufferedFrames(jitterBuffer.size())
                        recordOrderedBufferedFrames(
                            jitterBuffer.firstSequence()
                                ?.let { jitterBuffer.orderedSizeFrom(it) }
                                ?: 0
                        )
                    } else {
                        recordDiscardedPacket()
                    }
                } catch (error: SocketTimeoutException) {
                    if (running.get() && receiverRunning.get()) {
                        receiverError.set(error)
                    }
                    break
                } catch (_: Exception) {
                    if (running.get() && receiverRunning.get()) continue
                    break
                }
            }
            jitterBuffer.stop()
        }

        var expectedSequence: Long? = null
        var stretchedFrameBuffer = ByteArray(0)
        fun recordBufferStats() {
            val orderedHeadSequence = expectedSequence ?: jitterBuffer.firstSequence()
            recordBufferedFrames(jitterBuffer.size())
            recordOrderedBufferedFrames(
                orderedHeadSequence?.let { jitterBuffer.orderedSizeFrom(it) } ?: 0
            )
        }

        try {
            if (targetBufferFrames.get() > 0) {
                updateNotification("Buffering $host:$controlPort ${bufferMs}ms, latency=${latencyMs}ms")
                while (running.get()) {
                    val currentTargetBufferFrames = targetBufferFrames.get()
                    if (currentTargetBufferFrames <= 0) break
                    jitterBuffer.waitForFrames(currentTargetBufferFrames)
                    recordBufferStats()
                    throwReceiverErrorIfNeeded(receiverError)
                    if (jitterBuffer.size() >= targetBufferFrames.get()) break
                }
            } else {
                jitterBuffer.waitForFrames(1)
                recordBufferStats()
                throwReceiverErrorIfNeeded(receiverError)
            }

            updateNotification("Playing $host:$controlPort")

            while (running.get()) {
                val firstSequence = jitterBuffer.firstSequence() ?: run {
                    throwReceiverErrorIfNeeded(receiverError)
                    jitterBuffer.waitForFrames(1)
                    recordBufferStats()
                    throwReceiverErrorIfNeeded(receiverError)
                    continue
                }
                if (expectedSequence == null) expectedSequence = firstSequence

                val sequence = expectedSequence ?: firstSequence
                var frame = jitterBuffer.take(sequence)
                if (
                    frame == null &&
                    jitterBuffer.waitForSequenceOrFec(
                        sequence,
                        packetTimeoutMs(frameMs, jitterBuffer.size(), targetBufferFrames.get())
                    )
                ) {
                    frame = jitterBuffer.take(sequence)
                }
                throwReceiverErrorIfNeeded(receiverError)
                if (frame == null) {
                    jitterBuffer.fecFor(sequence)?.let { fecFrame ->
                        writeFully(resources.track, fecFrame.data, 0, fecFrame.length)
                        jitterBuffer.release(fecFrame)
                        duplicateFilter.markHandled(sequence)
                        expectedSequence = sequence + 1
                        recordBufferStats()
                        continue
                    }
                    val timedOutPackets = jitterBuffer.firstSequence()
                        ?.let { firstAvailable -> missingFrameCount(sequence, firstAvailable) }
                        ?.takeIf { it > 0 }
                        ?: 1
                    recordLostPackets(timedOutPackets)
                    recordTimedOutPackets(timedOutPackets)
                    if (autoBuffer) {
                        val updatedTargetFrames = autoBufferController.increaseForTimeout(
                            timedOutPackets.coerceAtMost(Int.MAX_VALUE.toLong()).toInt()
                        )
                        val previousTargetFrames = targetBufferFrames.getAndSet(updatedTargetFrames)
                        if (previousTargetFrames != updatedTargetFrames) {
                            setStatsTargetBufferFrames(updatedTargetFrames)
                            Log.i(
                                LOG_TAG,
                                "auto playback buffer increased after timeout " +
                                    "target=${updatedTargetFrames}frames/" +
                                    "${updatedTargetFrames * frameMs}ms"
                            )
                        }
                    }
                    expectedSequence = sequence + timedOutPackets
                    recordBufferStats()
                    continue
                }

                try {
                    val orderedBufferedFrames = jitterBuffer.orderedSizeFrom(sequence + 1) + 1
                    val currentTargetBufferFrames = targetBufferFrames.get()
                    val stretchRatio = playbackStretchRatio(
                        orderedBufferedFrames,
                        currentTargetBufferFrames
                    )
                    recordPlaybackStretchRatio(stretchRatio)
                    maybeLogPlaybackStretch(
                        orderedBufferedFrames,
                        currentTargetBufferFrames,
                        currentTargetBufferFrames * frameMs,
                        stretchRatio
                    )
                    if (stretchRatio > 1.0) {
                        val stretchedLength = stretchedPcm16StereoLength(frame.length, stretchRatio)
                        if (stretchedFrameBuffer.size < stretchedLength) {
                            stretchedFrameBuffer = ByteArray(stretchedLength)
                        }
                        stretchPcm16StereoLinear(
                            frame.data,
                            frame.length,
                            stretchedFrameBuffer,
                            stretchedLength
                        )
                        writeFully(resources.track, stretchedFrameBuffer, 0, stretchedLength)
                    } else {
                        writeFully(resources.track, frame.data, 0, frame.length)
                    }
                } finally {
                    jitterBuffer.release(frame)
                }
                expectedSequence = sequence + 1
                recordBufferStats()
            }
        } finally {
            receiverRunning.set(false)
            jitterBuffer.stop()
            recordBufferedFrames(0)
            recordOrderedBufferedFrames(0)
            resources.receiver.close()
            try {
                receiverThread.join(1000)
            } catch (_: InterruptedException) {
                Thread.currentThread().interrupt()
            }
        }
    }

    private fun throwReceiverErrorIfNeeded(receiverError: AtomicReference<Exception?>) {
        receiverError.get()?.let { throw it }
    }

    private fun receivePcmPayload(
        socket: DatagramSocket,
        packet: DatagramPacket
    ): PcmPayload? {
        packet.length = packet.data.size
        socket.receive(packet)
        val data = packet.data
        val length = packet.length
        if (length <= UDP_HEADER_BYTES) return null
        if (!hasUdpMagic(data)) return null
        if ((data[4].toInt() and 0xff) != UDP_VERSION) return null
        val headerLength = data[5].toInt() and 0xff
        if (headerLength < UDP_HEADER_BYTES || length <= headerLength) return null
        val primaryLength = readLittleEndianInt(data, 32)
        val fecSequence = readLittleEndianLong(data, 36)
        val fecCodec = data[44].toInt() and 0xff
        val fecLength = readLittleEndianShort(data, 46)
        if (primaryLength <= 0 || headerLength + primaryLength > length) return null
        val fecOffset = headerLength + primaryLength
        val hasFec = fecCodec == UDP_PACKET_FEC_CODEC_PCM_S16LE_STEREO && fecLength > 0
        if (hasFec && fecOffset + fecLength > length) return null
        return PcmPayload(
            sequence = readLittleEndianLong(data, 8),
            offset = headerLength,
            length = primaryLength,
            fecSequence = fecSequence,
            fecOffset = fecOffset,
            fecLength = if (hasFec) fecLength else 0,
            fecCodec = fecCodec
        )
    }

    private fun isNextOrFutureSequence(sequence: Long, expectedSequence: Long?): Boolean {
        return expectedSequence == null || sequence >= expectedSequence
    }

    private fun missingFrameCount(expectedSequence: Long?, sequence: Long): Long {
        if (expectedSequence == null || sequence <= expectedSequence) return 0
        return sequence - expectedSequence
    }

    private fun packetTimeoutMs(
        frameMs: Int,
        bufferedFrames: Int,
        targetBufferFrames: Int
    ): Long {
        val baseTimeoutMs = frameMs.toLong() * PACKET_TIMEOUT_FRAME_MULTIPLIER
        if (targetBufferFrames <= 1) {
            return baseTimeoutMs.coerceAtLeast(MIN_PACKET_TIMEOUT_MS)
        }

        val lowWatermarkFrames = (targetBufferFrames / PACKET_TIMEOUT_LOW_WATERMARK_DIVISOR)
            .coerceAtLeast(1)
        val missingWatermarkFrames = (lowWatermarkFrames - bufferedFrames).coerceAtLeast(0)
        val waterRecoveryMs = missingWatermarkFrames.toLong() * frameMs / 2
        return (baseTimeoutMs + waterRecoveryMs)
            .coerceIn(MIN_PACKET_TIMEOUT_MS, MAX_PACKET_TIMEOUT_MS)
    }

    private fun hasUdpMagic(data: ByteArray): Boolean {
        for (index in UDP_MAGIC.indices) {
            if (data[index] != UDP_MAGIC[index]) return false
        }
        return true
    }

    private fun readLittleEndianLong(data: ByteArray, offset: Int): Long {
        var value = 0L
        for (index in 0 until 8) {
            value = value or (
                (data[offset + index].toLong() and 0xffL) shl (index * 8)
            )
        }
        return value
    }

    private fun readLittleEndianInt(data: ByteArray, offset: Int): Int {
        var value = 0
        for (index in 0 until 4) {
            value = value or ((data[offset + index].toInt() and 0xff) shl (index * 8))
        }
        return value
    }

    private fun readLittleEndianShort(data: ByteArray, offset: Int): Int {
        return (data[offset].toInt() and 0xff) or
            ((data[offset + 1].toInt() and 0xff) shl 8)
    }

    private fun sendControlCommand(host: String, port: Int, command: String) {
        Socket().use { socket ->
            socket.tcpNoDelay = true
            socket.soTimeout = CONTROL_TIMEOUT_MS
            socket.connect(InetSocketAddress(host, port), CONNECT_TIMEOUT_MS)
            socket.getOutputStream().write("$command\n".toByteArray(Charsets.UTF_8))
            socket.getOutputStream().flush()

            val line = BufferedReader(InputStreamReader(socket.getInputStream(), Charsets.UTF_8))
                .readLine()
                ?: throw IllegalStateException("Empty control response")
            if (!line.startsWith("OK ")) {
                throw IllegalStateException(line.removePrefix("ERR ").ifBlank { line })
            }
        }
    }

    private fun bytesForDuration(durationMs: Int): Int {
        return ((SAMPLE_RATE * durationMs + 999) / 1000) * CHANNELS * BYTES_PER_SAMPLE
    }

    private fun fecBytesForDuration(durationMs: Int): Int {
        return bytesForDuration(durationMs)
    }

    private fun framesForDuration(durationMs: Int, frameMs: Int): Int {
        if (durationMs <= 0) return 0
        return ((durationMs + frameMs - 1) / frameMs).coerceAtLeast(1)
    }

    private fun jitterBufferOverflowHeadroomMs(bufferMs: Int): Int {
        return max(bufferMs, MAX_JITTER_BUFFER_EXTRA_MS)
    }

    private fun writeFully(track: AudioTrack, data: ByteArray, startOffset: Int, length: Int) {
        var offset = startOffset
        val endOffset = startOffset + length
        while (offset < endOffset && running.get()) {
            val written = track.write(data, offset, endOffset - offset)
            if (written <= 0) break
            offset += written
        }
    }

    private fun playbackStretchRatio(
        orderedBufferedFrames: Int,
        targetBufferFrames: Int
    ): Double {
        if (targetBufferFrames <= 0) return 1.0
        val bufferedPercent = orderedBufferedFrames * 100 / targetBufferFrames
        return when {
            bufferedPercent >= 90 -> PLAYBACK_STRETCH_90_PERCENT_RATIO
            bufferedPercent >= 80 -> PLAYBACK_STRETCH_80_PERCENT_RATIO
            bufferedPercent >= 70 -> PLAYBACK_STRETCH_70_PERCENT_RATIO
            bufferedPercent >= 60 -> PLAYBACK_STRETCH_60_PERCENT_RATIO
            bufferedPercent >= 50 -> PLAYBACK_STRETCH_50_PERCENT_RATIO
            else -> PLAYBACK_STRETCH_UNDER_50_PERCENT_RATIO
        }
    }

    private var lastStretchLogAtMs = 0L

    private fun maybeLogPlaybackStretch(
        orderedBufferedFrames: Int,
        targetBufferFrames: Int,
        targetBufferMs: Int,
        stretchRatio: Double
    ) {
        val nowMs = System.currentTimeMillis()
        if (nowMs - lastStretchLogAtMs < STRETCH_LOG_INTERVAL_MS) return
        lastStretchLogAtMs = nowMs
        Log.i(
            LOG_TAG,
            "playback stretch target=${targetBufferFrames}frames/${targetBufferMs}ms " +
                "orderedBuffered=${orderedBufferedFrames}frames ratio=$stretchRatio"
        )
    }

    private fun stretchedPcm16StereoLength(inputLength: Int, stretchRatio: Double): Int {
        val inputFrames = inputLength / PCM_FRAME_BYTES
        val outputFrames = (inputFrames * stretchRatio).roundToInt().coerceAtLeast(inputFrames)
        return outputFrames * PCM_FRAME_BYTES
    }

    private fun stretchPcm16StereoLinear(
        input: ByteArray,
        inputLength: Int,
        output: ByteArray,
        outputLength: Int
    ) {
        val inputFrames = inputLength / PCM_FRAME_BYTES
        val outputFrames = outputLength / PCM_FRAME_BYTES
        if (inputFrames <= 1 || outputFrames <= 1) {
            input.copyInto(output, 0, 0, inputLength.coerceAtMost(outputLength))
            return
        }

        val scale = (inputFrames - 1).toDouble() / (outputFrames - 1).toDouble()
        for (outputFrame in 0 until outputFrames) {
            val position = outputFrame * scale
            val frameIndex = position.toInt()
            val fraction = position - frameIndex
            val nextFrame = (frameIndex + 1).coerceAtMost(inputFrames - 1)
            for (channel in 0 until CHANNELS) {
                val sample = interpolatePcm16Sample(input, frameIndex, nextFrame, channel, fraction)
                writePcm16Le(output, (outputFrame * CHANNELS + channel) * BYTES_PER_SAMPLE, sample)
            }
        }
    }

    private fun interpolatePcm16Sample(
        input: ByteArray,
        frameIndex: Int,
        nextFrame: Int,
        channel: Int,
        fraction: Double
    ): Int {
        val first = readPcm16Le(input, (frameIndex * CHANNELS + channel) * BYTES_PER_SAMPLE)
        val second = readPcm16Le(input, (nextFrame * CHANNELS + channel) * BYTES_PER_SAMPLE)
        return (first + (second - first) * fraction)
            .roundToInt()
            .coerceIn(Short.MIN_VALUE.toInt(), Short.MAX_VALUE.toInt())
    }

    private fun readPcm16Le(data: ByteArray, offset: Int): Int {
        return (data[offset].toInt() and 0xff) or (data[offset + 1].toInt() shl 8)
    }

    private fun writePcm16Le(data: ByteArray, offset: Int, sample: Int) {
        data[offset] = sample.toByte()
        data[offset + 1] = (sample shr 8).toByte()
    }

    private fun playSilentStreamWarmup(
        host: String,
        controlPort: Int,
        resources: PlaybackResources,
        frameMs: Int
    ) {
        val maxPacketBytes = UDP_HEADER_BYTES + bytesForDuration(frameMs) + fecBytesForDuration(frameMs)
        val packetBuffer = ByteArray(maxPacketBytes)
        val silenceBuffer = ByteArray(bytesForDuration(frameMs))
        val datagram = DatagramPacket(packetBuffer, packetBuffer.size)
        sendControlCommand(host, controlPort, "START_UDP ${resources.receiver.localPort}")
        val deadlineNs = System.nanoTime() + REAL_STREAM_WARMUP_MS * 1_000_000L
        var expectedSequence: Long? = null
        try {
            while (running.get() && System.nanoTime() < deadlineNs) {
                val frame = receivePcmPayload(resources.receiver, datagram) ?: continue
                if (!isNextOrFutureSequence(frame.sequence, expectedSequence)) continue
                expectedSequence = frame.sequence + 1
                writeFully(resources.track, silenceBuffer, 0, frame.length)
            }
        } finally {
            runCatching { sendControlCommand(host, controlPort, "STOP_UDP") }
        }
    }

    private fun openPlaybackResources(targetBufferBytes: Int): PlaybackResources {
        val minBuffer = AudioTrack.getMinBufferSize(
            SAMPLE_RATE,
            AudioFormat.CHANNEL_OUT_STEREO,
            AudioFormat.ENCODING_PCM_16BIT
        )
        val trackBuffer = max(minBuffer, targetBufferBytes)
        val track = createAudioTrack(trackBuffer)
        audioTrack = track
        setStatsAudioTrack(track)
        if (trackBuffer > 0) {
            applyTargetBufferSize(track, trackBuffer)
        }
        track.play()

        val receiver = DatagramSocket(0).apply {
            try {
                receiveBufferSize = UDP_RECEIVE_BUFFER_BYTES
            } catch (_: Exception) {
            }
            soTimeout = UDP_TIMEOUT_MS
        }
        udpSocket = receiver
        return PlaybackResources(track, receiver)
    }

    private fun applyTargetBufferSize(track: AudioTrack, targetBufferBytes: Int) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.M || targetBufferBytes <= 0) {
            return
        }

        val targetFrames = targetBufferBytes / (CHANNELS * BYTES_PER_SAMPLE)
        if (targetFrames <= 0) {
            return
        }

        try {
            track.bufferSizeInFrames = targetFrames
        } catch (_: Exception) {
        }
    }

    private fun stopPlayback() {
        running.set(false)
        markPlaybackInactive()
        unregisterOutputDeviceChangeStop()
        closePlaybackResources()
        if (Thread.currentThread() != playbackThread) {
            try {
                playbackThread?.join(1000)
            } catch (_: InterruptedException) {
                Thread.currentThread().interrupt()
            }
        }
        playbackThread = null
    }

    private fun registerOutputDeviceChangeStopIfNeeded(stopOnChange: Boolean) {
        unregisterOutputDeviceChangeStop()
        if (!stopOnChange || Build.VERSION.SDK_INT < Build.VERSION_CODES.M) {
            return
        }

        val audioManager = getSystemService(Context.AUDIO_SERVICE) as AudioManager
        initialOutputDeviceSignature = currentOutputDeviceSignature(audioManager)
        val callback = object : AudioDeviceCallback() {
            override fun onAudioDevicesAdded(addedDevices: Array<out AudioDeviceInfo>) {
                stopIfOutputDevicesChanged(audioManager, stopOnChange)
            }

            override fun onAudioDevicesRemoved(removedDevices: Array<out AudioDeviceInfo>) {
                stopIfOutputDevicesChanged(audioManager, stopOnChange)
            }
        }

        outputDeviceCallback = callback
        audioManager.registerAudioDeviceCallback(callback, null)
    }

    private fun unregisterOutputDeviceChangeStop() {
        val callback = outputDeviceCallback ?: return
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) {
            val audioManager = getSystemService(Context.AUDIO_SERVICE) as AudioManager
            try {
                audioManager.unregisterAudioDeviceCallback(callback)
            } catch (_: Exception) {
            }
        }
        outputDeviceCallback = null
        initialOutputDeviceSignature = null
    }

    private fun stopIfOutputDevicesChanged(audioManager: AudioManager, enabled: Boolean) {
        if (!enabled) return
        val initial = initialOutputDeviceSignature ?: return
        if (currentOutputDeviceSignature(audioManager) == initial) return

        updateNotification("Stopping: audio output device changed")
        stopSelfWithPlayback(STOP_REASON_OUTPUT_DEVICE_CHANGED)
    }

    private fun stopSelfWithPlayback(reason: String) {
        stopPlayback()
        notifyPlaybackStopped(reason)
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    private fun notifyPlaybackStopped(reason: String) {
        if (playbackStoppedNotified) return
        playbackStoppedNotified = true
        sendBroadcast(Intent(ACTION_PLAYBACK_STOPPED).apply {
            setPackage(packageName)
            putExtra(EXTRA_STOP_REASON, reason)
        })
    }

    private fun currentOutputDeviceSignature(audioManager: AudioManager): Set<String> {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.M) return emptySet()
        return audioManager.getDevices(AudioManager.GET_DEVICES_OUTPUTS)
            .map { "${it.id}:${it.type}" }
            .toSet()
    }

    private fun closePlaybackResources() {
        try {
            udpSocket?.close()
        } catch (_: Exception) {
        }
        udpSocket = null

        val track = audioTrack
        recordClosedStatsAudioTrack(track)
        try {
            track?.pause()
            track?.flush()
            track?.release()
        } catch (_: Exception) {
        }
        audioTrack = null
    }

    private fun sleepBeforeReconnect(delayMs: Long) {
        var remainingMs = delayMs
        while (running.get() && remainingMs > 0) {
            val stepMs = remainingMs.coerceAtMost(100L)
            try {
                Thread.sleep(stepMs)
            } catch (_: InterruptedException) {
                Thread.currentThread().interrupt()
                return
            }
            remainingMs -= stepMs
        }
    }

    private fun setCurrentUdpReceiverThreadRealtimePriority() {
        try {
            Process.setThreadPriority(Process.THREAD_PRIORITY_URGENT_AUDIO )
        } catch (_: Exception) {
        }
    }

    private fun createAudioTrack(bufferSize: Int): AudioTrack {
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) {
            val builder = AudioTrack.Builder()
                .setAudioAttributes(
                    AudioAttributes.Builder()
                        .setUsage(AudioAttributes.USAGE_MEDIA)
                        .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                        .build()
                )
                .setAudioFormat(
                    AudioFormat.Builder()
                        .setSampleRate(SAMPLE_RATE)
                        .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                        .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO)
                        .build()
                )
                .setTransferMode(AudioTrack.MODE_STREAM)
                .setBufferSizeInBytes(bufferSize)

            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                builder.setPerformanceMode(AudioTrack.PERFORMANCE_MODE_LOW_LATENCY)
            }

            builder.build()
        } else {
            @Suppress("DEPRECATION")
            AudioTrack(
                AudioManager.STREAM_MUSIC,
                SAMPLE_RATE,
                AudioFormat.CHANNEL_OUT_STEREO,
                AudioFormat.ENCODING_PCM_16BIT,
                bufferSize,
                AudioTrack.MODE_STREAM
            )
        }
    }

    private fun createNotificationChannel() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val manager = getSystemService(NotificationManager::class.java)
            val channel = NotificationChannel(
                CHANNEL_ID,
                "Audio Link Playback",
                NotificationManager.IMPORTANCE_LOW
            )
            manager.createNotificationChannel(channel)
        }
    }

    private fun buildNotification(text: String): Notification {
        val openIntent = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
        )
        val stopIntent = PendingIntent.getService(
            this,
            1,
            Intent(this, PcmAudioService::class.java).apply { action = ACTION_STOP },
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
        )

        return NotificationCompat.Builder(this, CHANNEL_ID)
            .setSmallIcon(android.R.drawable.ic_media_play)
            .setContentTitle("Audio Link")
            .setContentText(text)
            .setOngoing(true)
            .setContentIntent(openIntent)
            .addAction(android.R.drawable.ic_media_pause, "Stop", stopIntent)
            .build()
    }

    private fun updateNotification(text: String) {
        val manager = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        manager.notify(NOTIFICATION_ID, buildNotification(text))
    }

    private inner class PlaybackResources(
        val track: AudioTrack,
        val receiver: DatagramSocket
    ) : AutoCloseable {
        override fun close() {
            recordClosedStatsAudioTrack(track)
            if (audioTrack === track) {
                audioTrack = null
            }
            if (udpSocket === receiver) {
                udpSocket = null
            }

            try {
                receiver.close()
            } catch (_: Exception) {
            }

            try {
                track.pause()
                track.flush()
                track.release()
            } catch (_: Exception) {
            }
        }
    }

    private class AutoBufferController(
        private val frameMs: Int,
        initialTargetFrames: Int
    ) {
        private val intervals = LongArray(AUTO_BUFFER_INTERVAL_WINDOW_SIZE)
        private var nextIntervalIndex = 0
        private var intervalCount = 0
        private var intervalTotalMs = 0L
        private var currentTargetFrames = initialTargetFrames.coerceAtLeast(minTargetFrames())
        private var lowerCandidateFrames = currentTargetFrames
        private var lowerCandidateSinceMs = 0L

        fun recordInterval(intervalMs: Long) {
            if (intervalMs <= 0) return

            if (intervalCount < intervals.size) {
                intervals[intervalCount] = intervalMs
                intervalTotalMs += intervalMs
                intervalCount += 1
            } else {
                intervalTotalMs -= intervals[nextIntervalIndex]
                intervals[nextIntervalIndex] = intervalMs
                intervalTotalMs += intervalMs
            }
            nextIntervalIndex = (nextIntervalIndex + 1) % intervals.size
        }

        fun targetFrames(): Int? {
            if (intervalCount <= 0) return null

            val nowMs = System.currentTimeMillis()
            val candidateFrames = calculatedTargetFrames()
            return when {
                candidateFrames >= currentTargetFrames -> {
                    currentTargetFrames = candidateFrames
                    lowerCandidateFrames = candidateFrames
                    lowerCandidateSinceMs = 0L
                    currentTargetFrames
                }
                candidateFrames != lowerCandidateFrames -> {
                    lowerCandidateFrames = candidateFrames
                    lowerCandidateSinceMs = nowMs
                    currentTargetFrames
                }
                lowerCandidateSinceMs > 0 &&
                    nowMs - lowerCandidateSinceMs >= AUTO_BUFFER_DECREASE_STABLE_MS -> {
                    currentTargetFrames = candidateFrames
                    lowerCandidateSinceMs = 0L
                    currentTargetFrames
                }
                else -> currentTargetFrames
            }
        }

        fun increaseForTimeout(missingFrames: Int): Int {
            val stepFrames = missingFrames.coerceAtLeast(1)
            currentTargetFrames = (currentTargetFrames + stepFrames)
                .coerceIn(minTargetFrames(), maxTargetFrames())
            lowerCandidateFrames = currentTargetFrames
            lowerCandidateSinceMs = 0L
            return currentTargetFrames
        }

        private fun calculatedTargetFrames(): Int {
            val averageMs = ceilDiv(intervalTotalMs, intervalCount.toLong())
            val p95Ms = percentile95Ms()
            val targetMs = max(
                max(averageMs * AUTO_BUFFER_AVG_MULTIPLIER, p95Ms * AUTO_BUFFER_P95_MULTIPLIER),
                MIN_AUTO_BUFFER_MS.toLong()
            )
            return framesForMs(targetMs).coerceIn(minTargetFrames(), maxTargetFrames())
        }

        private fun percentile95Ms(): Long {
            val samples = LongArray(intervalCount)
            for (index in 0 until intervalCount) {
                samples[index] = intervals[index]
            }
            samples.sort()
            val percentileIndex = ((intervalCount * 95 + 99) / 100 - 1)
                .coerceIn(0, intervalCount - 1)
            return samples[percentileIndex]
        }

        private fun framesForMs(durationMs: Long): Int {
            return ceilDiv(durationMs, frameMs.toLong()).toInt()
        }

        private fun minTargetFrames(): Int {
            return framesForMs(MIN_AUTO_BUFFER_MS.toLong()).coerceAtLeast(1)
        }

        private fun maxTargetFrames(): Int {
            return framesForMs(MAX_BUFFER_MS.toLong()).coerceAtLeast(1)
        }

        private fun ceilDiv(value: Long, divisor: Long): Long {
            if (divisor <= 0) return 0
            if (value <= 0) return 0
            return (value + divisor - 1) / divisor
        }
    }

    private class JitterBuffer(maxFrames: Int, frameBytes: Int, fecBytes: Int) {
        private val slots = Array(maxFrames.coerceAtLeast(1)) {
            JitterSlot(ByteArray(frameBytes), ByteArray(fecBytes.coerceAtLeast(1)))
        }
        private var bufferedFrames = 0
        private var stopped = false

        @Synchronized
        fun put(frame: PcmPayload, packet: ByteArray): Boolean {
            if (
                stopped ||
                frame.length > slots[0].data.size ||
                frame.fecLength > slots[0].fecData.size
            ) return false

            val slot = slots[slotIndex(frame.sequence)]
            if (slot.valid && slot.sequence == frame.sequence) return false
            if (slot.inUse) return false
            if (!slot.valid) bufferedFrames += 1
            packet.copyInto(slot.data, 0, frame.offset, frame.offset + frame.length)
            slot.sequence = frame.sequence
            slot.length = frame.length
            if (
                frame.fecCodec == UDP_PACKET_FEC_CODEC_PCM_S16LE_STEREO &&
                frame.fecLength > 0
            ) {
                packet.copyInto(slot.fecData, 0, frame.fecOffset, frame.fecOffset + frame.fecLength)
                slot.fecSequence = frame.fecSequence
                slot.fecLength = frame.fecLength
                slot.fecValid = true
            } else {
                slot.fecValid = false
                slot.fecLength = 0
            }
            slot.valid = true
            (this as java.lang.Object).notifyAll()
            return true
        }

        @Synchronized
        fun take(sequence: Long): JitterFrame? {
            dropBefore(sequence)
            val index = slotIndex(sequence)
            val slot = slots[index]
            if (!slot.valid || slot.sequence != sequence) return null
            slot.inUse = true
            bufferedFrames -= 1
            return JitterFrame(index, slot.data, slot.length, false)
        }

        @Synchronized
        fun fecFor(sequence: Long): JitterFrame? {
            for (slot in slots) {
                if (!slot.valid || slot.inUse || !slot.fecValid) continue
                if (slot.fecSequence != sequence) continue
                slot.inUse = true
                return JitterFrame(slotIndex(slot.sequence), slot.fecData, slot.fecLength, true)
            }
            return null
        }

        @Synchronized
        fun release(frame: JitterFrame) {
            val slot = slots[frame.slotIndex]
            if (!frame.fecOnly) {
                slot.valid = false
                slot.fecValid = false
            }
            slot.inUse = false
            (this as java.lang.Object).notifyAll()
        }

        @Synchronized
        fun firstSequence(): Long? {
            var first: Long? = null
            for (slot in slots) {
                if (!slot.valid || slot.inUse) continue
                val sequence = slot.sequence
                if (first == null || sequence < first) first = sequence
            }
            return first
        }

        @Synchronized
        fun size(): Int = bufferedFrames

        @Synchronized
        fun orderedSizeFrom(sequence: Long): Int {
            var orderedFrames = 0
            var currentSequence = sequence
            while (orderedFrames < bufferedFrames && contains(currentSequence)) {
                orderedFrames += 1
                currentSequence += 1
            }
            return orderedFrames
        }

        @Synchronized
        fun waitForFrames(targetFrames: Int) {
            while (!stopped && bufferedFrames < targetFrames) {
                try {
                    (this as java.lang.Object).wait(20)
                } catch (_: InterruptedException) {
                    Thread.currentThread().interrupt()
                    return
                }
            }
        }

        @Synchronized
        fun waitForSequence(sequence: Long, timeoutMs: Long): Boolean {
            val deadlineNs = System.nanoTime() + timeoutMs * 1_000_000L
            while (!stopped && !contains(sequence)) {
                val remainingNs = deadlineNs - System.nanoTime()
                if (remainingNs <= 0) break
                val waitMs = (remainingNs / 1_000_000L).coerceAtLeast(1L)
                try {
                    (this as java.lang.Object).wait(waitMs)
                } catch (_: InterruptedException) {
                    Thread.currentThread().interrupt()
                    return contains(sequence)
                }
            }
            return contains(sequence)
        }

        @Synchronized
        fun waitForSequenceOrFec(sequence: Long, timeoutMs: Long): Boolean {
            val deadlineNs = System.nanoTime() + timeoutMs * 1_000_000L
            while (!stopped && !contains(sequence) && !hasFec(sequence)) {
                val remainingNs = deadlineNs - System.nanoTime()
                if (remainingNs <= 0) break
                val waitMs = (remainingNs / 1_000_000L).coerceAtLeast(1L)
                try {
                    (this as java.lang.Object).wait(waitMs)
                } catch (_: InterruptedException) {
                    Thread.currentThread().interrupt()
                    return contains(sequence) || hasFec(sequence)
                }
            }
            return contains(sequence) || hasFec(sequence)
        }

        @Synchronized
        fun stop() {
            stopped = true
            for (slot in slots) {
                slot.valid = false
                slot.inUse = false
            }
            bufferedFrames = 0
            (this as java.lang.Object).notifyAll()
        }

        private fun dropBefore(sequence: Long) {
            for (slot in slots) {
                if (slot.valid && !slot.inUse && slot.sequence < sequence) {
                    slot.valid = false
                    bufferedFrames -= 1
                }
            }
        }

        private fun contains(sequence: Long): Boolean {
            val slot = slots[slotIndex(sequence)]
            return slot.valid && !slot.inUse && slot.sequence == sequence
        }

        private fun hasFec(sequence: Long): Boolean {
            for (slot in slots) {
                if (slot.valid && !slot.inUse && slot.fecValid && slot.fecSequence == sequence) {
                    return true
                }
            }
            return false
        }

        private fun slotIndex(sequence: Long): Int {
            return (sequence.mod(slots.size.toLong())).toInt()
        }

        private class JitterSlot(
            val data: ByteArray,
            val fecData: ByteArray,
            var sequence: Long = -1,
            var length: Int = 0,
            var fecSequence: Long = -1,
            var fecLength: Int = 0,
            var fecValid: Boolean = false,
            var valid: Boolean = false,
            var inUse: Boolean = false
        )

        class JitterFrame(
            val slotIndex: Int,
            val data: ByteArray,
            val length: Int,
            val fecOnly: Boolean
        )
    }

    private data class PcmPayload(
        val sequence: Long,
        val offset: Int,
        val length: Int,
        val fecSequence: Long,
        val fecOffset: Int,
        val fecLength: Int,
        val fecCodec: Int
    )

    private class DuplicatePacketFilter(capacity: Int) {
        private val ring = LongArray(capacity.coerceAtLeast(1))
        private val seen = HashSet<Long>(ring.size)
        private var nextIndex = 0

        fun isDuplicate(sequence: Long): Boolean {
            if (!seen.add(sequence)) return true

            remember(sequence)
            return false
        }

        fun markHandled(sequence: Long) {
            if (!seen.add(sequence)) return
            remember(sequence)
        }

        private fun remember(sequence: Long) {
            if (seen.size > ring.size) {
                seen.remove(ring[nextIndex])
            }
            ring[nextIndex] = sequence
            nextIndex = (nextIndex + 1) % ring.size
        }
    }
}
