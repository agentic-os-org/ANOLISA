import QtQuick
import Quickshell
import "plugin" as Integration

// Run with Quickshell: its static plugins cannot load in qmltestrunner.
ShellRoot {
    id: test
    property real homeHeight: 0
    property real footerClickY: 0
    property bool trackingNavigation: false
    property real smallestHeight: 0
    Integration.AssistantPopup {
        id: popup
        anchorItem: panel
        bar: null
        // Check real popup geometry without grabbing the desktop's focus.
        triggerMode: "hover"
        contentHeight: fittedContentHeight(panel.implicitHeight)
        onImplicitHeightChanged: {
            if (test.trackingNavigation) {
                test.smallestHeight = Math.min(test.smallestHeight, implicitHeight)
                console.log("Navigation surface height: " + implicitHeight)
            }
        }
    }
    FloatingWindow {
        visible: true
        implicitWidth: 400
        implicitHeight: 850
        Integration.AssistantPanel {
            id: panel
            anchors.fill: parent
            report: ({ok: true, setup: {needs_prompt: false},
                cosh: {choice: "skipped", verification: {result: "failed", checked_at: "2026-11-18T09:06:50+00:00"}},
                qoder: {installation: "compatible", configuration: "unknown", enhancement: "configured"}})
        }
    }
    function check(condition, message) {
        if (!condition) throw new Error(message)
        console.log("PASS: " + message)
    }
    Timer {
        interval: 50
        repeat: true
        running: true
        property int phase: 0
        onTriggered: {
            try {
                // Column implicit sizes settle on the next event-loop pass.
                if (phase === 0) {
                    test.homeHeight = panel.implicitHeight
                    popup.open = true
                    test.footerClickY = popup.implicitHeight - 20
                    test.smallestHeight = popup.implicitHeight
                    test.trackingNavigation = true
                    popup.holdHeight()
                    panel.page = "help"
                    phase = 1
                    return
                }
                var helpHeight = panel.implicitHeight
                console.log("Page heights: home=" + test.homeHeight + ", help=" + helpHeight)
                test.trackingNavigation = false
                check(test.smallestHeight > test.footerClickY,
                    "Home to Help keeps the original footer click inside the surface")
                check(popup.implicitHeight === Math.round(test.homeHeight),
                    "Help keeps the original popup height")
                popup.contentHeight = 720
                check(popup.implicitHeight === Math.round(test.homeHeight),
                    "Longer Help content does not resize the surface")
                popup.contentHeight = 420
                check(popup.implicitHeight === Math.round(test.homeHeight),
                    "Shorter Help content does not resize the surface")
                popup.releaseHeight()
                check(popup.implicitHeight === 420, "Leaving Help restores content sizing")
                popup.holdHeight()
                popup.contentHeight = 500
                popup.open = false
                popup.open = true
                check(popup.implicitHeight === 500, "Reopening resets the height")
                check(popup.fittedContentHeight(900, 400) === 400,
                    "Content still respects an explicit height cap")
                console.log("POPUP_GEOMETRY_OK")
            } catch (error) {
                console.error("POPUP_GEOMETRY_FAILED: " + error.message)
            }
            stop()
            popup.open = false
            Qt.quit()
        }
    }
}
