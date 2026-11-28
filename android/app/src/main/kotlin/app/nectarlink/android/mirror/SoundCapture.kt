// SPDX-License-Identifier: GPL-3.0-or-later
package app.nectarlink.android.mirror

import android.Manifest
import android.annotation.SuppressLint
import android.content.Context
import android.content.pm.PackageManager
import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioPlaybackCaptureConfiguration
import android.media.AudioRecord
import android.media.projection.MediaProjection
import android.os.Build
import android.os.SystemClock
import android.util.Log
import androidx.core.content.ContextCompat
import app.nectarlink.android.elevated.Elevated
import app.nectarlink.android.elevated.ElevatedAudioStream
import app.nectarlink.core.MirrorSendResult
import app.nectarlink.core.MirrorStream
import app.nectarlink.core.VideoPacketKind
import app.nectarlink.core.mirrorAudioConfig
import app.nectarlink.services.client.NectarlinkServiceContract

/**
 * Streams phone playback as 48 kHz stereo PCM in 10 ms packets. Uses the
 * Elevated helper's fixed-gain submix when available and Android 10+
 * [AudioPlaybackCaptureConfiguration] when sharing through MediaProjection.
 */
class SoundCapture(
    private val projection: MediaProjection?,
    private val stream: MirrorStream,
    private val allowElevated: Boolean = true,
    /** Called once the sound stops (the PC stopped listening, or capture failed). */
    private val onEnded: () -> Unit = {},
) {
    @Volatile private var running = false
    @Volatile private var elevatedSocket: ElevatedAudioStream? = null

    /** Whether any sound got as far as being sent. */
    @Volatile
    var sentSound = false
        private set

    fun start() {
        running = true
        Thread({
            try {
                if (running && allowElevated && Elevated.hasCapability(NectarlinkServiceContract.CAP_DEVICE_AUDIO)) {
                    try {
                        Elevated.openAudioStream()?.use { socket ->
                            elevatedSocket = socket
                            try {
                                capture(socket.inputStream::read)
                            } finally {
                                elevatedSocket = null
                            }
                        }
                    } catch (e: Exception) {
                        if (running) Log.w(TAG, "elevated sound capture stopped", e)
                    }
                }
                if (running && !sentSound && projection != null && Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                    captureProjection(projection)
                }
            } catch (e: Exception) {
                if (running) Log.w(TAG, "sound capture stopped", e)
            } finally {
                running = false
                stream.end()
                onEnded()
            }
        }, "sound-capture").apply {
            priority = Thread.MAX_PRIORITY
            start()
        }
    }

    @androidx.annotation.RequiresApi(Build.VERSION_CODES.Q)
    @SuppressLint("MissingPermission")
    private fun captureProjection(projection: MediaProjection) {
        val config = AudioPlaybackCaptureConfiguration.Builder(projection)
            .addMatchingUsage(AudioAttributes.USAGE_MEDIA)
            .addMatchingUsage(AudioAttributes.USAGE_GAME)
            .addMatchingUsage(AudioAttributes.USAGE_UNKNOWN)
            .build()
        val format = AudioFormat.Builder()
            .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
            .setSampleRate(RATE)
            .setChannelMask(AudioFormat.CHANNEL_IN_STEREO)
            .build()
        val minimum = AudioRecord.getMinBufferSize(RATE, AudioFormat.CHANNEL_IN_STEREO, AudioFormat.ENCODING_PCM_16BIT)
        val record = AudioRecord.Builder()
            .setAudioFormat(format)
            .setBufferSizeInBytes(maxOf(minimum, PACKET_BYTES * 8))
            .setAudioPlaybackCaptureConfig(config)
            .build()
        try {
            if (!running) return
            record.startRecording()
            capture(record::read)
        } finally {
            record.release()
        }
    }

    private fun capture(read: (ByteArray, Int, Int) -> Int) {
        val packet = ByteArray(PACKET_BYTES)
        var started = 0L
        while (running) {
            var filled = 0
            while (filled < packet.size && running) {
                val count = read(packet, filled, packet.size - filled)
                if (count <= 0) return
                filled += count
            }
            if (!running) return
            if (!sentSound) {
                sentSound = true
                started = SystemClock.elapsedRealtimeNanos()
                if (stream.send(VideoPacketKind.CONFIG, 0u, mirrorAudioConfig(RATE.toUInt(), CHANNELS.toUByte())) == MirrorSendResult.CLOSED) return
            }
            val time = ((SystemClock.elapsedRealtimeNanos() - started) / 1000).toULong()
            if (stream.send(VideoPacketKind.FRAME, time, packet.copyOf()) == MirrorSendResult.CLOSED) return
        }
    }

    fun stop() {
        running = false
        runCatching { elevatedSocket?.close() }
    }

    companion object {
        private const val TAG = "SoundCapture"
        private const val RATE = 48_000
        private const val CHANNELS = 2
        /** 10 ms of 16-bit stereo. */
        private const val PACKET_BYTES = RATE / 100 * CHANNELS * 2

        /** Whether this phone can share its sound (Elevated helper or Android 10+ with RECORD_AUDIO). */
        fun canCapture(context: Context): Boolean =
            Elevated.hasCapability(NectarlinkServiceContract.CAP_DEVICE_AUDIO) ||
                (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q &&
                    ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED)
    }
}
