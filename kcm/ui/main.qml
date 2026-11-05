// SPDX-License-Identifier: MIT

import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as QQC2
import org.kde.kirigami as Kirigami
import org.kde.kcmutils as KCM

KCM.SimpleKCM {
    id: root

    // only shown when something went wrong; changes apply live otherwise
    header: Kirigami.InlineMessage {
        position: Kirigami.InlineMessage.Position.Header
        type: Kirigami.MessageType.Error
        showCloseButton: false
        visible: kcm.errorText !== ""
        text: kcm.errorText
    }

    Kirigami.FormLayout {
        RowLayout {
            Kirigami.FormData.label: i18nc("@label:slider", "Tracking speed:")
            Layout.fillWidth: true
            spacing: Kirigami.Units.smallSpacing

            QQC2.Label {
                text: i18nc("@label:slider slow end of the tracking speed slider", "Slow")
            }
            QQC2.Slider {
                Layout.fillWidth: true
                Layout.minimumWidth: Kirigami.Units.gridUnit * 12
                // about as tall as a switch row, so the rows are evenly spaced
                Layout.preferredHeight: Math.max(implicitHeight, Kirigami.Units.gridUnit * 1.5)
                Accessible.name: i18nc("@label:slider", "Tracking speed")
                from: 0
                to: kcm.pointerMaxPosition
                stepSize: 1
                snapMode: QQC2.Slider.SnapAlways
                value: kcm.pointerPosition
                onMoved: kcm.setPointerPosition(Math.round(value))
            }
            QQC2.Label {
                text: i18nc("@label:slider fast end of the tracking speed slider", "Fast")
            }
        }

        RowLayout {
            Kirigami.FormData.label: i18nc("@label:slider", "Scroll speed:")
            Layout.fillWidth: true
            spacing: Kirigami.Units.smallSpacing

            QQC2.Label {
                text: i18nc("@label:slider slow end of the scroll speed slider", "Slow")
            }
            QQC2.Slider {
                Layout.fillWidth: true
                Layout.minimumWidth: Kirigami.Units.gridUnit * 12
                // about as tall as a switch row, so the rows are evenly spaced
                Layout.preferredHeight: Math.max(implicitHeight, Kirigami.Units.gridUnit * 1.5)
                Accessible.name: i18nc("@label:slider", "Scroll speed")
                from: 0
                to: kcm.scrollMaxPosition
                stepSize: 1
                snapMode: QQC2.Slider.SnapAlways
                value: kcm.scrollPosition
                onMoved: kcm.setScrollPosition(Math.round(value))
            }
            QQC2.Label {
                text: i18nc("@label:slider fast end of the scroll speed slider", "Fast")
            }
        }

        // a little room between the sliders and the switches
        Item {
            Kirigami.FormData.isSection: true
        }

        QQC2.Switch {
            text: i18nc("@option:check", "Use inertia when scrolling")
            checked: kcm.momentum
            onToggled: kcm.setMomentum(checked)
        }

        QQC2.Switch {
            text: i18nc("@option:check", "Three finger drag")
            checked: kcm.threeFingerDrag
            onToggled: kcm.setThreeFingerDrag(checked)
        }
    }
}
