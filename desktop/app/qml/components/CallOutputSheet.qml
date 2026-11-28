// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick
import app.nectarlink

Sheet {
    id: sheet
    cardWidth: 440
    readonly property string title: qsTr("Output for calls")
    readonly property var outputs: {
        const systemDefault = [{ id: "", name: qsTr("System default") }]
        try { return systemDefault.concat(JSON.parse(PhoneCall.callOutputs)) } catch (e) { return systemDefault }
    }
    readonly property var output: outputs.find(device => device.id === PhoneCall.callOutput) || outputs[0]

    function openForCalls() {
        PhoneCall.refreshCallOutputs()
        open()
    }

    Column {
        width: parent.width
        spacing: 12
        Txt { width: parent.width; text: sheet.title; role: "headline"; wrapMode: Text.WordWrap }
        Repeater {
            model: sheet.outputs
            delegate: Button {
                required property var modelData
                readonly property bool selected: modelData.id === sheet.output.id
                width: parent.width
                variant: selected ? "tonal" : "text"
                iconPath: selected ? Icons.check : Icons.speaker
                text: modelData.name
                Accessible.checkable: true
                Accessible.checked: selected
                onClicked: {
                    PhoneCall.chooseCallOutput(modelData.id)
                    sheet.close()
                }
            }
        }
        Row {
            anchors.right: parent.right
            Button { variant: "text"; text: qsTr("Close"); onClicked: sheet.close() }
        }
    }
}
