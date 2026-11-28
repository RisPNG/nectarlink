// SPDX-License-Identifier: GPL-3.0-or-later
package app.nectarlink.android.mirror

import android.content.Context
import android.os.Build
import androidx.core.content.edit
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/** Local playback preference for the next standalone phone sound session. */
object PhoneSoundPlayback {
    private const val PREFS = "phone_sound_playback"
    private const val KEEP_ON_PHONE = "keep_on_phone"

    private val current = MutableStateFlow(false)
    private val observable = current.asStateFlow()
    private var loaded = false

    val supported: Boolean
        get() = Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q

    @Synchronized
    fun state(context: Context): StateFlow<Boolean> {
        if (!loaded) {
            current.value = supported && prefs(context).getBoolean(KEEP_ON_PHONE, false)
            loaded = true
        }
        return observable
    }

    fun keepOnPhone(context: Context): Boolean = state(context).value

    @Synchronized
    fun setKeepOnPhone(context: Context, keepOnPhone: Boolean) {
        if (!supported) return
        prefs(context).edit { putBoolean(KEEP_ON_PHONE, keepOnPhone) }
        current.value = keepOnPhone
        loaded = true
    }

    private fun prefs(context: Context) =
        context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
}
