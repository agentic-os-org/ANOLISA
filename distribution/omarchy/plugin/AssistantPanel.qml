import QtQuick
import QtQuick.Layouts

// Shared by the native popup and the local renderer; previews use this exact view.
Rectangle {
    id: panel
    objectName: "assistantPanel"
    property real unit: 1
    property string typeface: "Sans Serif"
    property string page: "home"
    property var report: ({})
    property bool busy: false
    property string failure: ""
    readonly property var setupState: report.setup || ({})
    readonly property var coshState: report.cosh || ({})
    readonly property var qoderState: report.qoder || ({})
    readonly property string language: (report.ui || ({})).language || "en-US"
    signal languageRequested(string language)
    signal actionRequested(string action)
    signal navigateRequested(string page)
    signal closeRequested()
    signal refreshRequested()
    implicitWidth: 400 * unit
    implicitHeight: 58 * unit + body.implicitHeight + 18 * unit + 40 * unit
    color: "#21262c"
    radius: 16 * unit
    border.color: "#3a4145"
    border.width: 1

    // The backend supplies the selected user's catalog; system locale is not used.
    function tr(message) {
        var strings = (report.ui || ({})).strings || ({})
        return language === "zh-CN" && strings[message] !== undefined ? strings[message] : message
    }

    function count(value) {
        return value.toLocaleString(Qt.locale("en_US"), "f", 0)
    }
    function metric(component, field) {
        var data = report[component] || ({})
        return data.available && typeof data[field] === "number" ? count(data[field]) : "—"
    }
    function componentStatus(component) {
        if (!report.ok) return panel.tr("Status unknown")
        var data = report[component] || ({})
        if (data.state === "empty" || (data.available && data.checked === 0)) return panel.tr("No records yet")
        return data.available ? panel.tr("Records available") : panel.tr("Unavailable")
    }
    function choiceLabel(value) {
        if (value.verification && value.verification.checked_at)
            return (value.verification.result === "passed" ? panel.tr("Response confirmed by user") : panel.tr("Connection failure reported by user"))
                + " · " + value.verification.checked_at
        if (value.reported_configured) return panel.tr("Manually configured; connection not verified")
        if (value.choice === "skipped") return panel.tr("Skipped; set up later")
        if (value.choice === "configure") return panel.tr("Configured; connection not verified")
        return panel.tr("Not configured")
    }

    component Label: Text {
        color: "#eef1ed"
        font.family: panel.typeface
        font.pixelSize: 13 * panel.unit
        wrapMode: Text.Wrap
        textFormat: Text.PlainText
        lineHeight: 1.45
    }
    component ComponentRow: Rectangle {
        id: row
        property string name: ""
        property string description: ""
        property string symbol: ""
        property string destination: ""
        width: parent.width
        height: 62 * panel.unit
        color: hover.containsMouse ? "#29312f" : "transparent"
        radius: 7 * panel.unit
        Accessible.role: Accessible.Button
        Accessible.name: name + "，" + description
        Accessible.onPressAction: panel.navigateRequested(destination)
        activeFocusOnTab: true
        Keys.onReturnPressed: panel.navigateRequested(destination)
        Keys.onSpacePressed: panel.navigateRequested(destination)
        Label {
            x: 6 * panel.unit
            anchors.verticalCenter: parent.verticalCenter
            text: row.symbol
            color: "#b7d9be"
            font.pixelSize: 20 * panel.unit
        }
        Column {
            x: 38 * panel.unit
            anchors.verticalCenter: parent.verticalCenter
            spacing: 2 * panel.unit
            Label { text: row.name; font.pixelSize: 14 * panel.unit; font.weight: Font.DemiBold }
            Label { text: row.description; color: "#a3ada8"; font.pixelSize: 12 * panel.unit }
        }
        Label {
            anchors.right: parent.right
            anchors.rightMargin: 10 * panel.unit
            anchors.verticalCenter: parent.verticalCenter
            text: panel.componentStatus(row.destination) + "  ›"
            color: "#a3b7a9"
            font.pixelSize: 12 * panel.unit
        }
        Rectangle { anchors.bottom: parent.bottom; width: parent.width; height: 1; color: "#30373b" }
        MouseArea {
            id: hover
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: panel.navigateRequested(row.destination)
        }
    }

    RowLayout {
        x: 18 * panel.unit
        y: 13 * panel.unit
        width: parent.width - 36 * panel.unit
        spacing: 8 * panel.unit
        Label { text: "✦"; color: "#b7d9be"; font.pixelSize: 23 * panel.unit }
        Label {
            objectName: "productTitle"
            text: panel.tr("ANOLISA - AI Assistant")
            font.pixelSize: 16 * panel.unit
            font.weight: Font.DemiBold
            fontSizeMode: Text.HorizontalFit
            minimumPixelSize: 12 * panel.unit
            wrapMode: Text.NoWrap
            elide: Text.ElideRight
            Layout.minimumWidth: 0
            Layout.fillWidth: true
        }
        ActionButton {
            objectName: "settingsButton"
            text: panel.page === "home" ? "⚙" : panel.tr("‹ Back")
            textSize: panel.page === "home" ? 18 : 12
            quiet: true; unit: panel.unit; typeface: panel.typeface
            onClicked: panel.navigateRequested(panel.page === "home" ? "settings" : "home")
        }
        ActionButton {
            objectName: "closeButton"
            text: "×"; quiet: true; unit: panel.unit; typeface: panel.typeface
            textSize: 20
            onClicked: panel.closeRequested()
        }
    }
    Flickable {
        id: viewport
        objectName: "panelViewport"
        x: 18 * panel.unit
        y: 58 * panel.unit
        width: parent.width - 36 * panel.unit
        height: Math.max(0, parent.height - y - 48 * panel.unit)
        contentWidth: width
        contentHeight: body.implicitHeight
        clip: true
        boundsBehavior: Flickable.StopAtBounds
        onHeightChanged: contentY = Math.min(contentY, Math.max(0, contentHeight - height))
        Connections { target: panel; function onPageChanged() { viewport.contentY = 0 } }
        Column {
            id: body
            width: viewport.width
            spacing: 12 * panel.unit
            Label { objectName: "failureMessage"; visible: panel.failure !== ""; width: parent.width; text: panel.tr(panel.failure); color: "#efd199" }
            Column {
                visible: panel.page === "home" && panel.setupState.needs_prompt === true
                width: parent.width
                spacing: 8 * panel.unit
                Label { width: parent.width; text: panel.tr("Welcome"); font.bold: true }
                Label { width: parent.width; text: panel.tr("Set up cosh and Qoder separately. You can finish either later."); color: "#a3ada8" }
                RowLayout {
                    width: parent.width
                    ActionButton {
                        objectName: "setupButton"
                        text: panel.tr("Start setup"); unit: panel.unit; typeface: panel.typeface
                        enabled: !panel.setupState.running
                        onClicked: panel.actionRequested("setup")
                    }
                    ActionButton {
                        objectName: "deferButton"
                        text: panel.tr("Later"); quiet: true; unit: panel.unit; typeface: panel.typeface
                        enabled: !panel.setupState.running
                        onClicked: panel.actionRequested("defer")
                    }
                }
            }
            Column {
                visible: panel.page === "home" && panel.setupState.needs_prompt !== true
                width: parent.width
                spacing: 12 * panel.unit
                Label { width: parent.width; text: panel.tr("Start AI work and review enhancement results."); color: "#a3ada8" }
                Rectangle {
                    width: parent.width
                    implicitHeight: work.implicitHeight + 28 * panel.unit
                    height: implicitHeight
                    radius: 10 * panel.unit
                    color: "#262c31"
                    border.color: "#363e42"
                    Column {
                        id: work
                        x: 14 * panel.unit; y: 14 * panel.unit
                        width: parent.width - 28 * panel.unit
                        spacing: 9 * panel.unit
                        Label { text: panel.tr("Start work"); color: "#a3b7a9"; font.pixelSize: 11 * panel.unit }
                        Label { text: panel.tr("Open work terminal"); font.pixelSize: 18 * panel.unit; font.weight: Font.DemiBold }
                        Label { width: parent.width; text: "cosh · " + panel.choiceLabel(panel.coshState); color: "#a3ada8" }
                        ActionButton {
                            objectName: "coshButton"
                            width: parent.width
                            text: panel.tr("Open work terminal →")
                            primary: true; unit: panel.unit; typeface: panel.typeface
                            onClicked: panel.actionRequested("cosh")
                        }
                    }
                }
                ActionButton {
                    objectName: "qoderButton"
                    width: parent.width
                    quiet: true; unit: panel.unit; typeface: panel.typeface
                    text: panel.qoderState.installation !== "compatible" || panel.qoderState.configuration === "missing"
                        ? panel.tr("Set up Qoder →") : panel.qoderState.enhancement === "unavailable"
                        ? panel.tr("Check Qoder →") : panel.tr("Use Qoder →")
                    onClicked: {
                        if (panel.qoderState.installation !== "compatible" || panel.qoderState.configuration === "missing")
                            panel.actionRequested("setup-qoder")
                        else if (panel.qoderState.enhancement === "unavailable")
                            panel.navigateRequested("settings")
                        else
                            panel.actionRequested("qoder")
                    }
                }
                Label {
                    width: parent.width
                    text: panel.tr("Follow Qoder tasks and confirmations in the terminal. Review enhancements below.")
                    color: "#a3ada8"; font.pixelSize: 11 * panel.unit
                }
                RowLayout {
                    width: parent.width
                    Label { text: panel.tr("Components and dashboards"); color: "#a3ada8"; font.pixelSize: 12 * panel.unit; Layout.fillWidth: true }
                    ActionButton { text: panel.tr("Settings"); quiet: true; unit: panel.unit; typeface: panel.typeface; onClicked: panel.navigateRequested("settings") }
                }
                Column {
                    width: parent.width
                    ComponentRow { objectName: "securityRow"; name: "sec-core"; description: panel.tr("Security checks"); symbol: "◇"; destination: "security" }
                    ComponentRow { objectName: "tokenlessRow"; name: "Tokenless"; description: panel.tr("Reduce token usage"); symbol: "≋"; destination: "tokenless" }
                }
            }
            Column {
                visible: panel.page === "settings"
                width: parent.width
                spacing: 10 * panel.unit
                Label { text: panel.tr("Settings"); font.pixelSize: 18 * panel.unit; font.bold: true }
                Label { objectName: "languageLabel"; text: panel.tr("Language"); font.bold: true }
                RowLayout {
                    width: parent.width
                    ActionButton {
                        objectName: "englishLanguageButton"
                        text: "English"; primary: panel.language === "en-US"
                        enabled: !panel.busy
                        unit: panel.unit; typeface: panel.typeface
                        onClicked: panel.languageRequested("en-US")
                    }
                    ActionButton {
                        objectName: "chineseLanguageButton"
                        text: panel.tr("Simplified Chinese"); primary: panel.language === "zh-CN"
                        enabled: !panel.busy
                        unit: panel.unit; typeface: panel.typeface
                        onClicked: panel.languageRequested("zh-CN")
                    }
                }
                Label {
                    width: parent.width
                    text: "cosh · " + panel.choiceLabel(panel.coshState) + "\nQoder · "
                        + panel.choiceLabel(panel.qoderState)
                        + (panel.qoderState.version ? " · " + panel.qoderState.version : "")
                        + (panel.qoderState.choice === "configure"
                            ? (panel.qoderState.auth_mode === "pat" ? panel.tr(" · PAT saved by plugin") : panel.tr(" · Account managed in Qoder CLI"))
                            : "")
                    color: "#a3ada8"
                }
                Label {
                    width: parent.width
                    visible: panel.qoderState.enhancement === "unavailable"
                    text: panel.tr("Safety checks or installation settings are unavailable. Run diagnostics for details.")
                    color: "#efd199"
                }
                ActionButton { objectName: "setupCoshButton"; text: panel.tr("Set up cosh"); width: parent.width; unit: panel.unit; typeface: panel.typeface; onClicked: panel.actionRequested("setup-cosh") }
                ActionButton { objectName: "setupQoderButton"; text: panel.tr("Set up Qoder"); width: parent.width; unit: panel.unit; typeface: panel.typeface; onClicked: panel.actionRequested("setup-qoder") }
                ActionButton { objectName: "qoderVersionButton"; text: panel.tr("Qoder version"); width: parent.width; unit: panel.unit; typeface: panel.typeface; onClicked: panel.actionRequested("qoder-install") }
                Label { width: parent.width; visible: panel.qoderState.build_validation === "not_verified"; text: panel.tr("Latest build; plugin compatibility not verified"); color: "#efd199" }
                ActionButton { objectName: "doctorButton"; text: panel.tr("Run diagnostics"); width: parent.width; unit: panel.unit; typeface: panel.typeface; onClicked: panel.actionRequested("doctor") }
                Label { visible: panel.setupState.running === true; width: parent.width; text: panel.tr("Setup is already running. Return to its terminal."); color: "#a3ada8" }
            }
            Column {
                visible: panel.page === "security" || panel.page === "tokenless"
                width: parent.width
                spacing: 14 * panel.unit
                Label {
                    width: parent.width
                    text: panel.page === "security" ? panel.tr("sec-core · Security checks") : panel.tr("Tokenless · Reduce token usage")
                    font.pixelSize: 18 * panel.unit; font.weight: Font.DemiBold
                }
                Rectangle {
                    width: parent.width
                    height: metrics.implicitHeight + 28 * panel.unit
                    color: "#262c31"; radius: 10 * panel.unit; border.color: "#363e42"
                    Column {
                        id: metrics
                        x: 14 * panel.unit; y: 14 * panel.unit
                        width: parent.width - 28 * panel.unit
                        spacing: 9 * panel.unit
                        Label { text: panel.page === "security" ? panel.tr("Recent scans") : panel.tr("Estimated tool-content reduction"); color: "#a3ada8" }
                        Label {
                            objectName: "metricValue"
                            width: parent.width
                            text: panel.page === "security" ? panel.metric("security", "checked") + panel.tr(" records") : panel.metric("tokenless", "saved") + panel.tr(" tokens")
                            color: "#b7d9be"; font.pixelSize: 28 * panel.unit; font.weight: Font.DemiBold
                        }
                        Label {
                            width: parent.width
                            text: panel.page === "security" ? panel.tr("Risks found: ") + panel.metric("security", "risks") + panel.tr("\nExecution blocks: ") + panel.metric("security", "blocked")
                                : panel.tr("Before: ") + panel.metric("tokenless", "before") + panel.tr(" → After: ") + panel.metric("tokenless", "after") + panel.tr("\nRecords: ") + panel.metric("tokenless", "records") + panel.tr(" records")
                            color: "#a3ada8"
                        }
                    }
                }
                Label {
                    width: parent.width
                    text: panel.page === "security"
                        ? (((panel.report.security || ({})).recent_risks || []).map(function(item) {
                            return (item.timestamp || panel.tr("Time unknown")) + " · " + (item.verdict === "deny" ? panel.tr("Deny recommendation") : panel.tr("Risk warning"))
                        }).join("\n") || (panel.componentStatus("security") === panel.tr("Unavailable") ? panel.tr("Scan records are currently unavailable.") : panel.tr("No risk details yet.")))
                        : panel.tr("Estimates cover tool content sent to the model, not total model usage or billing savings.")
                    color: "#a3ada8"
                }
                Label {
                    width: parent.width
                    text: panel.page === "security"
                        ? panel.tr("Up to 200 recent service scans. Covers Qoder Bash calls. Execution blocking requires separate evidence.")
                        : panel.tr("Up to 200 recent records for this user. Estimates compare tool content.")
                    color: "#a3ada8"; font.pixelSize: 12 * panel.unit
                }
                Label {
                    width: parent.width
                    text: panel.componentStatus(panel.page) + " · " + ((panel.report[panel.page] || ({})).sampled_at || panel.tr("Update time unknown"))
                    color: "#a3ada8"; font.pixelSize: 11 * panel.unit
                }
                ActionButton { width: parent.width; text: panel.tr("Refresh"); quiet: true; unit: panel.unit; typeface: panel.typeface; onClicked: panel.refreshRequested() }
                ActionButton {
                    objectName: "detailsButton"
                    width: parent.width; text: panel.tr("Full details in terminal →"); unit: panel.unit; typeface: panel.typeface
                    onClicked: panel.actionRequested(panel.page === "security" ? "security" : "tokens")
                }
            }
            Column {
                visible: panel.page === "help"
                width: parent.width
                spacing: 12 * panel.unit
                Label { objectName: "helpTitle"; text: panel.tr("Help"); font.pixelSize: 18 * panel.unit; font.weight: Font.DemiBold }
                Label { text: panel.tr("Run in a terminal"); font.weight: Font.DemiBold }
                Label { objectName: "helpCommands"; width: parent.width; text: panel.tr("anolisa-omarchy help\nLearn what the components do and how to open them.\n\nanolisa-omarchy status\nCheck installation, configuration and service status.\n\nanolisa-omarchy doctor\nCheck installation and runtime issues.") }
                Label { text: panel.tr("What the components do"); font.weight: Font.DemiBold }
                Label { width: parent.width; text: panel.tr("Qoder · Read files, run commands and edit code.") }
                Label { width: parent.width; text: panel.tr("cosh · An AI work terminal. Configure a model with /auth, then enter /agent on its own to ask questions.") }
                Label { width: parent.width; text: panel.tr("Tokenless · Shorten supported Qoder tool outputs to reduce tokens sent to the model.") }
                Label { width: parent.width; text: panel.tr("sec-core · Check Qoder Bash commands before execution and record detected risks.") }
            }
        }
    }
    Rectangle {
        anchors.bottom: parent.bottom
        width: parent.width
        height: 40 * panel.unit
        color: "transparent"
        Rectangle { width: parent.width - 2; x: 1; height: 1; color: "#30373b" }
        Label {
            x: 18 * panel.unit; anchors.verticalCenter: parent.verticalCenter
            text: panel.busy ? panel.tr("Updating…") : panel.tr("ANOLISA · Local data")
            color: "#81978a"; font.pixelSize: 10 * panel.unit
        }
        ActionButton {
            objectName: "helpButton"
            anchors.right: parent.right; anchors.rightMargin: 12 * panel.unit
            anchors.verticalCenter: parent.verticalCenter
            text: panel.tr("Help"); quiet: true; unit: panel.unit; typeface: panel.typeface
            onClicked: panel.navigateRequested("help")
        }
    }
}
