#!/usr/bin/env bash
set -euo pipefail

source_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
test_dir=$(mktemp -d)
trap 'rm -rf -- "$test_dir"' EXIT

# Quickshell resolves qs imports from its config root, so stage the real QML
# files beside the test theme rather than mocking the popup implementation.
mkdir -p "$test_dir/plugin"
cp "$source_dir/tests/quickshell/shell.qml" "$test_dir/"
cp -r "$source_dir/tests/quickshell/Commons" "$test_dir/"
cp "$source_dir/plugin/AssistantPanel.qml" "$source_dir/plugin/ActionButton.qml" "$test_dir/plugin/"
cp "${POPUP_SOURCE:-$source_dir/plugin/AssistantPopup.qml}" "$test_dir/plugin/AssistantPopup.qml"
QT_QPA_PLATFORM=offscreen QT_QUICK_BACKEND=software \
  timeout 15 quickshell -p "$test_dir" --no-color 2>&1 | tee "$test_dir/result.log"
grep -q 'POPUP_GEOMETRY_OK' "$test_dir/result.log"
