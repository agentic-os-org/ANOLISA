pragma Singleton
import QtQuick

// Only the native popup's theme dependency is replaced in geometry tests.
QtObject {
    readonly property int gapsOut: 8
    function space(value) { return value }
}
