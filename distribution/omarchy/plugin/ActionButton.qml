import QtQuick

Rectangle {
    id: button
    property string text: ""
    property bool primary: false
    property bool quiet: false
    property real unit: 1
    property string typeface: "Sans Serif"
    property real textSize: quiet ? 12 : 14
    signal clicked()
    implicitWidth: label.implicitWidth + 24 * unit
    implicitHeight: (quiet ? 28 : 40) * unit
    radius: 7 * unit
    color: primary ? (pointer.containsMouse ? "#c5e5cb" : "#b7d9be")
        : pointer.containsMouse ? "#303a35" : "transparent"
    border.width: primary || quiet ? 0 : 1
    border.color: "#49534e"
    activeFocusOnTab: true
    Accessible.role: Accessible.Button
    Accessible.name: text
    Accessible.onPressAction: clicked()
    Keys.onReturnPressed: clicked()
    Keys.onSpacePressed: clicked()
    Text {
        id: label
        anchors.centerIn: parent
        text: button.text
        color: button.primary ? "#1b3325" : "#b7d9be"
        font.family: button.typeface
        font.pixelSize: button.textSize * button.unit
        font.weight: button.primary ? Font.DemiBold : Font.Normal
    }
    Rectangle {
        anchors.fill: parent
        anchors.margins: -2 * button.unit
        visible: button.activeFocus
        color: "transparent"
        radius: button.radius + 2 * button.unit
        border.color: "#b7d9be"
    }
    MouseArea {
        id: pointer
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: button.clicked()
    }
}
