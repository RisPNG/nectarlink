// SPDX-License-Identifier: GPL-3.0-or-later
package app.nectarlink.android.calls

import android.content.Context
import android.media.AudioManager
import android.os.Build
import android.util.Log
import androidx.core.content.edit

object CallEarpiece {
    private const val TAG = "CallEarpiece"
    private const val PREFS = "call_earpiece"
    private const val KEY_VOLUME = "volume"

    /** Restores the saved volume from an older build after an interrupted call. */
    fun end(context: Context) {
        val prefs = prefs(context)
        val volume = prefs.getInt(KEY_VOLUME, -1)
        if (volume < 0) return
        prefs.edit(commit = true) { remove(KEY_VOLUME) }
        val audio = context.getSystemService(AudioManager::class.java) ?: return
        if (audio.getStreamVolume(AudioManager.STREAM_VOICE_CALL) == lowest(audio)) {
            runCatching { audio.setStreamVolume(AudioManager.STREAM_VOICE_CALL, volume, 0) }
                .onFailure { Log.w(TAG, "can't give the call volume back", it) }
        }
    }

    /** The lowest call volume; before Android 9 it can't be asked, and is 1. */
    private fun lowest(audio: AudioManager): Int =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) audio.getStreamMinVolume(AudioManager.STREAM_VOICE_CALL) else 1

    private fun prefs(context: Context) = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
}
