// SPDX-License-Identifier: GPL-3.0-or-later
pragma Singleton
import QtQuick

// Stands in for the Rust PhoneCall in QML tests: no call.
QtObject {
    property bool active: false
    property bool ringing: false
    property bool held: false
    property bool muted: false
    property bool canEnd: false
    property bool controls: false
    property string caller: ""
    property string number: ""
    property string callOutputs: "[]"
    property string callOutput: ""
    property int callPlaybackVolume: 100
    property int outputRefreshes: 0
    property int savedCallVolume: -1

    function answer() {}
    function end() {}
    function setMute(on) {}
    function refreshCallOutputs() { outputRefreshes++ }
    function chooseCallOutput(id) { callOutput = id }
    function setCallPlaybackVolume(level, done) {
        callPlaybackVolume = level
        if (done)
            savedCallVolume = level
    }
}
