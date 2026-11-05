import QtQuick
import QtQuick.Layouts
import QtQuick.Controls

ApplicationWindow {
    id: root

    FontMetrics { id: fm }
    SystemPalette { id: pal }

    // one "unit" is a line of text: sizes follow the font, not pixels
    readonly property real unit: fm.height
    readonly property real margin: unit
    readonly property real gridWidth: Math.min(width - 2 * margin, unit * 24)

    title: qsTr("Apple Magic Trackpad")
    color: pal.window
    width: unit * 28
    minimumWidth: unit * 22

    // A fixed height sized to the content: the rows of controls have nothing
    // to grow into. Tiling window managers may still stretch the window; the
    // controls then stay at the top.
    Component.onCompleted: {
        const h = Math.ceil(grid.implicitHeight + 2 * margin + footerBar.implicitHeight);
        minimumHeight = h;
        maximumHeight = h;
        height = h;
        visible = true;
    }

    GridLayout {
        id: grid
        anchors.horizontalCenter: parent.horizontalCenter
        anchors.top: parent.top
        anchors.topMargin: root.margin
        width: root.gridWidth
        columns: 2
        columnSpacing: root.unit * 0.75
        rowSpacing: root.unit

        Label {
            Layout.row: 0
            Layout.column: 0
            Layout.columnSpan: 2
            Layout.preferredWidth: root.gridWidth
            Layout.preferredHeight: contentHeight
            visible: !backend.speedsAvailable
            wrapMode: Text.WordWrap
            text: qsTr("Tracking and scroll speed need KDE Plasma or Hyprland. Use your desktop's own settings.")
        }

        Label {
            Layout.row: 1
            Layout.column: 0
            Layout.alignment: Qt.AlignRight | Qt.AlignVCenter
            enabled: backend.speedsAvailable
            text: qsTr("Tracking speed")
        }
        RowLayout {
            Layout.row: 1
            Layout.column: 1
            Layout.fillWidth: true
            enabled: backend.speedsAvailable
            Label { text: qsTr("Slow") }
            Slider {
                Layout.fillWidth: true
                from: 0
                to: backend.pointerMaxPosition
                stepSize: 1
                snapMode: Slider.SnapAlways
                value: backend.pointerPosition
                onMoved: backend.setPointerPosition(Math.round(value))
            }
            Label { text: qsTr("Fast") }
        }

        Label {
            Layout.row: 2
            Layout.column: 0
            Layout.alignment: Qt.AlignRight | Qt.AlignVCenter
            enabled: backend.speedsAvailable
            text: qsTr("Scroll speed")
        }
        RowLayout {
            Layout.row: 2
            Layout.column: 1
            Layout.fillWidth: true
            enabled: backend.speedsAvailable
            Label { text: qsTr("Slow") }
            Slider {
                Layout.fillWidth: true
                from: 0
                to: backend.scrollMaxPosition
                stepSize: 1
                snapMode: Slider.SnapAlways
                value: backend.scrollPosition
                onMoved: backend.setScrollPosition(Math.round(value))
            }
            Label { text: qsTr("Fast") }
        }

        Switch {
            Layout.row: 3
            Layout.column: 1
            text: qsTr("Use inertia when scrolling")
            checked: backend.momentum
            onToggled: backend.setMomentum(checked)
        }

        Switch {
            Layout.row: 4
            Layout.column: 1
            text: qsTr("Three finger drag")
            checked: backend.threeFingerDrag
            onToggled: backend.setThreeFingerDrag(checked)
        }
    }

    footer: ToolBar {
        id: footerBar
        position: ToolBar.Footer
        contentItem: Label {
            text: backend.status
            color: backend.statusError ? "#e53935" : pal.windowText
            elide: Text.ElideRight
            verticalAlignment: Text.AlignVCenter
            ToolTip.visible: statusHover.hovered && truncated
            ToolTip.text: backend.status
            HoverHandler { id: statusHover }
        }
    }
}
