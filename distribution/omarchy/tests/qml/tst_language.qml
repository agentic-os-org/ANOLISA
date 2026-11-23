import QtQuick
import QtTest
import "../../plugin" as Integration

TestCase {
    id: test
    name: "AssistantLanguage"
    when: windowShown
    width: 400
    height: 850
    visible: true
    property var catalog: ({})

    Integration.AssistantPanel {
        id: panel
        anchors.fill: parent
    }
    SignalSpy { id: languageSpy; target: panel; signalName: "languageRequested" }
    Connections {
        target: panel
        function onNavigateRequested(page) { panel.page = page }
    }

    function report(language) {
        return {ok: true, ui: {language: language, strings: language === "zh-CN" ? catalog.ui : {}},
            setup: {needs_prompt: false}, cosh: {choice: "configure"},
            qoder: {installation: "compatible", configuration: "unknown", enhancement: "configured"}}
    }
    function initTestCase() {
        // Read the shipped catalog rather than duplicating translations in the test.
        var request = new XMLHttpRequest()
        request.open("GET", Qt.resolvedUrl("../../translations.json"), false)
        request.send()
        catalog = JSON.parse(request.responseText)
        verify(catalog.ui.Settings === "设置")
    }
    function init() {
        panel.page = "home"
        panel.busy = false
        panel.failure = ""
        panel.report = report("en-US")
        languageSpy.clear()
        wait(20)
    }
    function test_default_english() {
        panel.report = ({})
        compare(panel.language, "en-US")
        compare(findChild(panel, "productTitle").text, "ANOLISA - AI Assistant")
    }
    function test_switch_buttons_and_live_labels() {
        panel.page = "settings"
        var chinese = findChild(panel, "chineseLanguageButton")
        var english = findChild(panel, "englishLanguageButton")
        wait(50)
        verify(chinese.visible && chinese.width > 0 && chinese.height > 0,
            JSON.stringify({visible: chinese.visible, width: chinese.width, height: chinese.height,
                panel: panel.visible, test: test.visible, parent: chinese.parent.visible}))
        verify(english.primary)
        compare(findChild(panel, "languageLabel").text, "Language")
        compare(chinese.text, "Simplified Chinese")
        mouseClick(chinese)
        compare(languageSpy.count, 1)
        compare(languageSpy.signalArguments[0][0], "zh-CN")
        panel.report = report("zh-CN")
        compare(findChild(panel, "productTitle").text, "ANOLISA - AI 助手")
        verify(chinese.primary)
        compare(findChild(panel, "languageLabel").text, catalog.ui.Language)
        compare(chinese.text, catalog.ui["Simplified Chinese"])
        compare(findChild(panel, "setupCoshButton").text, "设置 cosh")
        panel.page = "help"
        verify(findChild(panel, "helpCommands").text.indexOf("查看组件用途") !== -1)
        panel.page = "settings"
        mouseClick(english)
        compare(languageSpy.count, 2)
        compare(languageSpy.signalArguments[1][0], "en-US")
        panel.report = report("en-US")
        compare(findChild(panel, "setupCoshButton").text, "Set up cosh")
    }
    function test_unverified_configuration_stays_unverified() {
        compare(panel.choiceLabel({choice: "configure"}), "Configured; connection not verified")
        panel.report = report("zh-CN")
        compare(panel.choiceLabel({choice: "configure"}), "已处理，连接待验证")
    }
    function test_error_translation() {
        panel.failure = "Cannot read status. Refresh or run diagnostics."
        compare(findChild(panel, "failureMessage").text, panel.failure)
        panel.report = report("zh-CN")
        compare(findChild(panel, "failureMessage").text, "无法读取状态，请刷新或运行检查。")
    }
    function test_help_navigation_data() {
        return [
            {tag: "home-en", language: "en-US", settings: false, label: "Help"},
            {tag: "settings-en", language: "en-US", settings: true, label: "Help"},
            {tag: "home-zh", language: "zh-CN", settings: false, label: "帮助"},
            {tag: "settings-zh", language: "zh-CN", settings: true, label: "帮助"}
        ]
    }
    function test_help_navigation(data) {
        panel.report = report(data.language)
        if (data.settings) {
            mouseClick(findChild(panel, "settingsButton"))
            compare(panel.page, "settings")
        }
        compare(findChild(panel, "helpButton").text, data.label)
        mouseClick(findChild(panel, "helpButton"))
        compare(panel.page, "help")
        verify(findChild(panel, "helpTitle").visible)
        compare(findChild(panel, "helpTitle").text, data.label)
        wait(250)
        compare(panel.page, "help")
        verify(findChild(panel, "helpCommands").visible)
    }
}
