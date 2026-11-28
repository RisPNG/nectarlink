// SPDX-License-Identifier: GPL-3.0-or-later
package app.nectarlink.android.elevated

import android.annotation.SuppressLint
import android.content.Context
import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioRecord
import android.os.Build
import android.os.Process
import app.nectarlink.android.BuildConfig
import java.io.IOException
import java.net.Socket
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.concurrent.thread

/** The helper's addressed Android playback copy, including eligible VoIP output. */
internal object DeviceAudioMix {
    const val COPY_COMMAND = "A KEEP"
    const val MIN_COPY_SDK = Build.VERSION_CODES.TIRAMISU

    @SuppressLint("MissingPermission", "PrivateApi", "DiscouragedPrivateApi")
    fun streamCopy(client: Socket) {
        if (Build.VERSION.SDK_INT < MIN_COPY_SDK) return
        try {
            val out = client.getOutputStream()
            val context = ShellContext.get()
            val audio = checkNotNull(context.getSystemService(AudioManager::class.java))
            val ruleType = Class.forName("android.media.audiopolicy.AudioMixingRule")
            val ruleBuilderType = Class.forName("android.media.audiopolicy.AudioMixingRule\$Builder")
            val ruleBuilder = ruleBuilderType.getConstructor().newInstance()
            val matchUsage = ruleType.getField("RULE_MATCH_ATTRIBUTE_USAGE").getInt(null)
            val matchUid = ruleType.getField("RULE_MATCH_UID").getInt(null)
            val addRule = ruleBuilderType.getMethod("addRule", AudioAttributes::class.java, Int::class.javaPrimitiveType)
            val exclude = ruleBuilderType.getMethod("excludeMixRule", Int::class.javaPrimitiveType, Any::class.java)
            // Android checks the helper's voice-capture permission when the mix explicitly matches this usage.
            val usages = intArrayOf(
                AudioAttributes.USAGE_UNKNOWN,
                AudioAttributes.USAGE_MEDIA,
                AudioAttributes.USAGE_GAME,
                AudioAttributes.USAGE_VOICE_COMMUNICATION,
            )
            for (usage in usages) {
                addRule.invoke(ruleBuilder, AudioAttributes.Builder().setUsage(usage).build(), matchUsage)
            }
            // Keep playback that NectarLink produced out of its own capture.
            exclude.invoke(ruleBuilder, matchUid, context.packageManager.getPackageUid(BuildConfig.APPLICATION_ID, 0))
            exclude.invoke(ruleBuilder, matchUid, Process.myUid())
            val rule = ruleBuilderType.getMethod("build").invoke(ruleBuilder)

            val rate = 48_000
            val format = AudioFormat.Builder().setSampleRate(rate)
                .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO)
                .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                .build()
            val mixType = Class.forName("android.media.audiopolicy.AudioMix")
            val mixBuilderType = Class.forName("android.media.audiopolicy.AudioMix\$Builder")
            val mixBuilder = mixBuilderType.getConstructor(ruleType).newInstance(rule)
            mixBuilderType.getMethod("setFormat", AudioFormat::class.java).invoke(mixBuilder, format)
            val loopback = mixType.getField("ROUTE_FLAG_LOOP_BACK").getInt(null)
            val render = mixType.getField("ROUTE_FLAG_RENDER").getInt(null)
            mixBuilderType.getMethod("setRouteFlags", Int::class.javaPrimitiveType).invoke(mixBuilder, loopback or render)
            val mix = mixBuilderType.getMethod("build").invoke(mixBuilder)

            val policyType = Class.forName("android.media.audiopolicy.AudioPolicy")
            val policyBuilderType = Class.forName("android.media.audiopolicy.AudioPolicy\$Builder")
            val policyBuilder = policyBuilderType.getConstructor(Context::class.java).newInstance(context)
            policyBuilderType.getMethod("addMix", mixType).invoke(policyBuilder, mix)
            val policy = policyBuilderType.getMethod("build").invoke(policyBuilder)
            val register = AudioManager::class.java.getMethod("registerAudioPolicy", policyType)
            val unregister = AudioManager::class.java.getMethod("unregisterAudioPolicy", policyType)
            check(register.invoke(audio, policy) == 0) { "Android refused the playback copy policy" }
            var record: AudioRecord? = null
            val finished = AtomicBoolean(false)
            try {
                val sink = policyType.getMethod("createAudioRecordSink", mixType).invoke(policy, mix) as? AudioRecord
                    ?: error("no playback copy recorder")
                record = sink
                check(sink.state == AudioRecord.STATE_INITIALIZED) { "playback copy recorder not initialized" }
                sink.startRecording()
                check(sink.recordingState == AudioRecord.RECORDSTATE_RECORDING) { "playback copy recorder did not start" }
                // The client only reads PCM. Closing its socket must also stop a silent recorder.
                thread(isDaemon = true, name = "device-audio-stop") {
                    runCatching { client.getInputStream().read() }
                    if (!finished.get()) runCatching { sink.stop() }
                }
                val buffer = ByteArray(rate / 100 * 2 * 2)
                var count = sink.read(buffer, 0, buffer.size)
                if (count <= 0) return
                // The app can fall back to its consent flow if capture fails before the acknowledgement.
                out.write("OK\n".toByteArray(Charsets.US_ASCII))
                while (count > 0) {
                    out.write(buffer, 0, count)
                    out.flush()
                    count = sink.read(buffer, 0, buffer.size)
                }
            } finally {
                finished.set(true)
                runCatching { record?.release() }
                unregister.invoke(audio, policy)
            }
        } catch (_: IOException) {
            // The app closed its capture socket.
        } catch (e: Exception) {
            System.err.println("can't copy phone playback: $e")
        }
    }
}
