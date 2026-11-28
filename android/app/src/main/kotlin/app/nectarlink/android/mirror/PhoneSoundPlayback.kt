// SPDX-License-Identifier: GPL-3.0-or-later
package app.nectarlink.android.mirror

import android.content.Context
import android.os.Build
import androidx.core.content.edit

/** Local playback preference for the next standalone phone sound session. */
object PhoneSoundPlayback {
    private const val PREFS = "phone_sound_playback"
    private const val KEEP_ON_PHONE = "keep_on_phone"

    val supported: Boolean
        get() = Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q

    fun keepOnPhone(context: Context): Boolean =
        supported && prefs(context).getBoolean(KEEP_ON_PHONE, false)

    fun setKeepOnPhone(context: Context, keepOnPhone: Boolean) {
        prefs(context).edit { putBoolean(KEEP_ON_PHONE, keepOnPhone) }
    }

    private fun prefs(context: Context) =
        context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
}
