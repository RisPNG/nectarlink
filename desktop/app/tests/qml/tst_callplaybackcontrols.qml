// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick
import QtTest
import app.nectarlink

Item {
    id: root
    width: 500
    height: 400

    CallVolumeSlider { id: homeVolume; x: 20; y: 20 }
    CallVolumeSlider { id: settingsVolume; x: 20; y: 80 }
    CallOutputSheet { id: outputSheet }

    function outputButton(item, id) {
        for (const child of item.children) {
            if (child.modelData !== undefined && child.modelData.id === id)
                return child
            const found = outputButton(child, id)
            if (found)
                return found
        }
        return null
    }

    TestCase {
        name: "CallPlaybackControls"
        when: windowShown

        function init() {
            PhoneCall.active = false
            PhoneCall.callOutputs = JSON.stringify([{ id: "headset", name: "Headset" }])
            PhoneCall.callOutput = ""
            PhoneCall.callPlaybackVolume = 100
            PhoneCall.outputRefreshes = 0
            PhoneCall.savedCallVolume = -1
        }

        function cleanup() { outputSheet.close() }

        function test_output_can_be_picked_before_a_call() {
            PhoneCall.callOutput = "unplugged-device"
            compare(outputSheet.output.id, "")
            outputSheet.openForCalls()
            verify(outputSheet.opened)
            compare(PhoneCall.outputRefreshes, 1)
            const headset = root.outputButton(outputSheet, "headset")
            verify(headset)
            headset.clicked()
            verify(!outputSheet.opened)
            compare(PhoneCall.callOutput, "headset")
            compare(outputSheet.output.id, "headset")
        }

        function test_volume_changes_on_both_pages_and_saves_after_movement() {
            homeVolume.forceActiveFocus()
            keyClick(Qt.Key_Left)
            compare(homeVolume.value, 95)
            compare(settingsVolume.value, 95)
            compare(PhoneCall.savedCallVolume, -1)
            tryCompare(PhoneCall, "savedCallVolume", 95)
        }
    }
}
