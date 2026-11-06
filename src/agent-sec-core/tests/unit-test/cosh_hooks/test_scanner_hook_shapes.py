# This file is a part of the open-eBackup project.
# This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0.
# If a copy of the MPL was not distributed with this file, You can obtain one at
# http://mozilla.org/MPL/2.0/.
#
# Copyright (c) [2024] Huawei Technologies Co.,Ltd.
#
# THIS SOFTWARE IS PROVIDED ON AN "AS IS" BASIS, WITHOUT WARRANTIES OF ANY KIND,
# EITHER EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO NON-INFRINGEMENT,
# MERCHANTABILITY OR FIT FOR A PARTICULAR PURPOSE.
"""cosh/codex 扫描钩子对畸形 CLI 输出的 fail-open 回归。

钩子 shell 出去调 PATH 上的 agent-sec-cli——版本漂移是常态。三个钩子
对输出形状不设防，崩溃后 exit 1 且无 HookOutput JSON，违反 fail-open
不变量。修复按同仓既有守卫对齐。
"""

import json
import sys
from pathlib import Path

from standalone_hook_test_loader import load_standalone_hook

_ROOT = Path(__file__).resolve().parents[3]
_HOOKS = _ROOT / "cosh-extension" / "hooks"
_CODEX_HOOKS = _ROOT / "codex-plugin" / "hooks-plugin" / "hooks"

cosh_code_scanner = load_standalone_hook(
    "cosh_code_scanner_hook", _HOOKS / "code_scanner_hook.py"
)
cosh_prompt_scanner = load_standalone_hook(
    "cosh_prompt_scanner_hook", _HOOKS / "prompt_scanner_hook.py"
)
codex_code_scanner = load_standalone_hook(
    "codex_code_scanner_hook", _CODEX_HOOKS / "code_scanner_hook.py"
)


class TestCoshCodeScannerFindingShapes:
    """cosh code_scanner_hook: findings 元素缺 desc_zh / findings 非列表不崩。"""

    def test_finding_without_desc_zh_falls_back_to_desc_en(self):
        """修复前：f['desc_zh'] 直接下标——英文-only CLI 输出 KeyError。"""
        result = json.loads(
            cosh_code_scanner._format_cosh(
                {
                    "verdict": "warn",
                    "findings": [
                        {"rule_id": "X001", "severity": "warn", "desc_en": "english only"}
                    ],
                }
            )
        )
        assert result["decision"] == "ask"
        assert "english only" in result["systemMessage"]

    def test_findings_null_treated_as_empty(self):
        """修复前：findings: null → TypeError: 'NoneType' is not iterable。"""
        result = json.loads(
            cosh_code_scanner._format_cosh({"verdict": "warn", "findings": None})
        )
        assert result["decision"] == "ask"
        assert "0 issue" in result["systemMessage"]

    def test_non_dict_finding_skipped(self):
        result = json.loads(
            cosh_code_scanner._format_cosh(
                {"verdict": "warn", "findings": ["not-a-dict"]}
            )
        )
        assert result["decision"] == "ask"


class TestCoshPromptScannerNonObjectJson:
    """cosh prompt_scanner_hook: 合法 JSON 但非对象走 fail-open。"""

    def test_format_cosh_none_is_allow(self):
        """修复前：_format_cosh 对非 dict 抛 AttributeError。对齐
        pii_checker_hook 的既有守卫：非对象按 None → fail-open allow。"""
        assert json.loads(cosh_prompt_scanner._format_cosh(None))["decision"] == "allow"
        assert (
            json.loads(cosh_prompt_scanner._format_cosh("plain string"))["decision"]
            == "allow"
        )


class TestCodexCodeScannerInputShapes:
    """codex code_scanner_hook: 非对象 tool_input fail-open。"""

    def test_string_tool_input_returns_cleanly(self):
        """修复前：tool_input.get 对 str 抛 AttributeError。
        同插件 pii_checker 文档化 tool_input 可以是 string/object/array。"""
        # main() 消费 stdin；用 monkeypatched stdin 太重——直接驱动 main，
        # 传入合法 JSON（verdict pass 路径不触发 subprocess）
        import io as _io

        stdin = json.dumps(
            {
                "hook_event_name": "PreToolUse",
                "tool_name": "Bash",
                "tool_input": "echo hello",
            }
        )
        old = sys.stdin
        try:
            sys.stdin = _io.StringIO(stdin)
            codex_code_scanner.main()  # 修复前 AttributeError，修复后干净返回
        finally:
            sys.stdin = old
