// SPDX-License-Identifier: GPL-3.0-or-later
package app.nectarlink.android.mirror

import android.content.Context
import android.media.AudioManager
import android.util.Log
import androidx.core.content.edit

/** Restores a media volume left at zero by a previous version of sound sharing. */
object PhoneQuiet {
    private const val TAG = "PhoneQuiet"
    private const val PREFS = "phone_quiet"
    private const val KEY_VOLUME = "volume"

    fun restore(context: Context) {
        val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        val volume = prefs.getInt(KEY_VOLUME, -1)
        if (volume < 0) return
        prefs.edit(commit = true) { remove(KEY_VOLUME) }
        val audio = context.getSystemService(AudioManager::class.java) ?: return
        if (audio.getStreamVolume(AudioManager.STREAM_MUSIC) == 0) {
            runCatching { audio.setStreamVolume(AudioManager.STREAM_MUSIC, volume, 0) }
                .onFailure { Log.w(TAG, "can't give the media volume back", it) }
        }
    }
}
