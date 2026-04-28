import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

Card {
    id: root

    property var device: null

    title: i18n("Battery")
    implicitHeight: Kirigami.Units.gridUnit * 8

    function formatBatteryTime(minutes) {
        if (!minutes || minutes <= 0) return ""

        const hours = Math.floor(minutes / 60)
        const mins = minutes % 60

        if (hours === 0) {
            return i18n("~%1m left", mins)
        } else if (hours === 1 && mins === 0) {
            return i18n("~1h left")
        } else if (mins === 0) {
            return i18n("~%1h left", hours)
        } else {
            return i18n("~%1h %2m left", hours, mins)
        }
    }

    function formatCaseLevel() {
        const caseBattery = device?.battery?.case
        if (!caseBattery) {
            return "--"
        }
        return i18n("%1%%", caseBattery.level ?? 0)
    }

    contentItem: Component {
        ColumnLayout {
            spacing: Kirigami.Units.smallSpacing

            // Battery indicators row
            RowLayout {
                spacing: Kirigami.Units.largeSpacing
                Layout.fillWidth: true

                // Single headphone battery (AirPods Max)
                CircularBatteryIndicator {
                    Layout.fillWidth: true
                    Layout.alignment: Qt.AlignCenter
                    visible: !!device?.battery?.headphone
                    label: i18n("MX")
                    level: device?.battery?.headphone?.level ?? 0
                    charging: !!device?.battery?.headphone?.charging
                    size: Kirigami.Units.gridUnit * 3.5
                    showEarStatus: true
                    inEar: !(!device?.ear_detection?.left_in_ear && !device?.ear_detection?.right_in_ear)
                }

                // Left AirPod
                CircularBatteryIndicator {
                    Layout.fillWidth: true
                    Layout.alignment: Qt.AlignCenter
                    visible: !!device?.battery?.left && !device?.battery?.headphone
                    label: i18n("L")
                    level: device?.battery?.left?.level ?? 0
                    charging: !!device?.battery?.left?.charging
                    size: Kirigami.Units.gridUnit * 3.5
                    showEarStatus: true
                    inEar: !!device?.ear_detection?.left_in_ear
                }

                // Right AirPod
                CircularBatteryIndicator {
                    Layout.fillWidth: true
                    Layout.alignment: Qt.AlignCenter
                    visible: !!device?.battery?.right && !device?.battery?.headphone
                    label: i18n("R")
                    level: device?.battery?.right?.level ?? 0
                    charging: !!device?.battery?.right?.charging
                    size: Kirigami.Units.gridUnit * 3.5
                    showEarStatus: true
                    inEar: !!device?.ear_detection?.right_in_ear
                }
            }

            // Case status (always visible, even when unavailable)
            RowLayout {
                Layout.fillWidth: true
                Layout.topMargin: Kirigami.Units.smallSpacing
                spacing: Kirigami.Units.smallSpacing

                Kirigami.Icon {
                    source: "battery"
                    Layout.preferredWidth: Kirigami.Units.iconSizes.smallMedium
                    Layout.preferredHeight: Kirigami.Units.iconSizes.smallMedium
                    opacity: 0.8
                }

                Text {
                    text: i18n("Case: %1", formatCaseLevel())
                    font.pixelSize: Kirigami.Units.gridUnit * 0.62
                    color: Kirigami.Theme.textColor
                    opacity: 0.8
                }

                Kirigami.Icon {
                    visible: !!device?.battery?.case?.charging
                    source: "battery-charging-symbolic"
                    Layout.preferredWidth: Kirigami.Units.iconSizes.small
                    Layout.preferredHeight: Kirigami.Units.iconSizes.small
                    color: "#4CAF50"
                }

                Item {
                    Layout.fillWidth: true
                }
            }

            // Battery TTL estimate
            Text {
                Layout.fillWidth: true
                Layout.alignment: Qt.AlignHCenter
                Layout.topMargin: Kirigami.Units.smallSpacing

                // Show only when estimate is available and no component is charging
                visible: {
                    const hasEstimate = device?.battery_ttl_estimate != null && device?.battery_ttl_estimate !== undefined
                    const headphoneCharging = device?.battery?.headphone?.charging ?? false
                    const leftCharging = device?.battery?.left?.charging ?? false
                    const rightCharging = device?.battery?.right?.charging ?? false
                    const notCharging = !(headphoneCharging || leftCharging || rightCharging)
                    return hasEstimate && notCharging
                }

                text: formatBatteryTime(device?.battery_ttl_estimate ?? 0)
                font.pixelSize: Kirigami.Units.gridUnit * 0.6
                font.weight: Font.Light
                color: Kirigami.Theme.textColor
                opacity: visible ? 0.6 : 0
                horizontalAlignment: Text.AlignHCenter

                // Gentle fade in/out when estimate becomes available or unavailable
                Behavior on opacity {
                    NumberAnimation {
                        duration: 500
                        easing.type: Easing.InOutQuad
                    }
                }
            }
        }
    }
}
