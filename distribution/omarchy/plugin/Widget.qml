import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Ui

BarWidget {
    id: root
    moduleName: "community.anolisa"
    property bool opened: false
    property bool busy: false
    property string page: "home"
    property string failure: ""
    property var report: ({})
    readonly property string bridge: Qt.resolvedUrl("bridge.py").toString().replace(/^file:\/\//, "")
    implicitWidth: pill.implicitWidth
    implicitHeight: pill.implicitHeight

    function tr(message) {
        var ui = report.ui || ({})
        var strings = ui.strings || ({})
        return ui.language === "zh-CN" && strings[message] !== undefined ? strings[message] : message
    }
    function clearReport() {
        // A failed status request must not change the user's selected interface language.
        report = {ui: report.ui || ({})}
    }
    function changeLanguage(language) {
        // A pending refresh must not overwrite the newly selected language with an old report.
        if (languageSetup.running || collector.running) return
        busy = true
        languageSetup.command = ["python3", bridge, "language", language]
        languageSetup.running = true
    }
    function refresh() {
        if (collector.running || languageSetup.running) return
        busy = true
        collector.command = ["python3", bridge, "status", "--json"]
        collector.running = true
    }
    function open() { page = "home"; opened = true; refresh() }
    function close() { opened = false }
    function terminal(action) {
        if (action === "defer") {
            if (!deferSetup.running) deferSetup.running = true
            return
        }
        var args = action === "setup-cosh" ? ["setup", "--component", "cosh"]
            : action === "setup-qoder" ? ["setup", "--component", "qoder"] : [action]
        Quickshell.execDetached(["omarchy-launch-terminal", "python3", bridge].concat(args))
        opened = false
    }
    IpcHandler {
        target: "community.anolisa"
        function open(): void { root.open() }
        function close(): void { root.close() }
        function status(): string { return JSON.stringify({opened: root.opened, page: root.page, busy: root.busy, failure: root.failure, report: root.report}) }
    }
    Process {
        id: collector
        stdout: StdioCollector {
            onStreamFinished: {
                try {
                    var next = JSON.parse(text)
                    if (next.schema_version !== 2 || !next.ok) throw new Error("status schema")
                    root.report = next
                    root.failure = ""
                } catch (error) {
                    root.clearReport()
                    root.failure = "Cannot read status. Refresh or run diagnostics."
                }
            }
        }
        onExited: function(code, status) {
            root.busy = false
            if (code !== 0) {
                root.clearReport()
                root.failure = "Status retrieval failed. Refresh or run diagnostics."
            }
        }
    }
    Process {
        id: deferSetup
        command: ["python3", root.bridge, "setup", "--defer"]
        onExited: function(code, status) {
            if (code === 0) root.refresh()
            else root.failure = "Could not defer setup. Close the setup terminal and try again."
        }
    }
    Process {
        id: languageSetup
        onExited: function(code, status) {
            root.busy = false
            if (code === 0) root.refresh()
            else root.failure = "Could not save the language. Try again."
        }
    }
    Timer { interval: 15000; running: root.opened; repeat: true; onTriggered: root.refresh() }
    WidgetButton {
        id: pill
        bar: root.bar
        text: "✦ ANOLISA"
        active: root.opened
        tooltipText: root.tr("ANOLISA - AI Assistant")
        onPressed: root.opened ? root.close() : root.open()
    }
    AssistantPopup {
        id: popup
        anchorItem: pill
        bar: root.bar
        owner: root
        open: root.opened
        contentWidth: fittedContentWidth(Style.space(400))
        contentHeight: fittedContentHeight(content.implicitHeight)
        AssistantPanel {
            id: content
            anchors.fill: parent
            unit: Style.space(100) / 100
            page: root.page
            report: root.report
            failure: root.failure
            busy: root.busy
            onActionRequested: function(action) { root.terminal(action) }
            onNavigateRequested: function(page) {
                if (page === "help") popup.holdHeight()
                root.page = page
                if (page !== "help") popup.releaseHeight()
                root.refresh()
            }
            onCloseRequested: root.close()
            onRefreshRequested: root.refresh()
            onLanguageRequested: function(language) { root.changeLanguage(language) }
        }
    }
}
