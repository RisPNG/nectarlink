// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick
import app.nectarlink

ValueSlider {
    value: PhoneCall.callPlaybackVolume
    label: qsTr("Call volume")
    valueText: qsTr("%1%").arg(value)
    onMoved: (next) => {
        PhoneCall.setCallPlaybackVolume(next, false)
        saveCallVolume.restart()
    }

    Timer {
        id: saveCallVolume
        interval: 600
        onTriggered: PhoneCall.setCallPlaybackVolume(PhoneCall.callPlaybackVolume, true)
    }
}
