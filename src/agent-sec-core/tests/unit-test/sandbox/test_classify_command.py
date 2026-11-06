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
#
"""CommandClassifier 的 deny 层与 safe 层回归测试。"""

from agent_sec_cli.sandbox.classify_command import CommandClassifier


def _decision(command: str) -> str:
    return CommandClassifier().classify(command)["decision"]


class TestShellWrapperDenyLayer:
    """bash -c 内含 shell 操作符时 deny 层不得整体跳过。

    修复前：_extract_shell_commands 见到 $(`/反引号/重定向/括号 即返回
    None，destructive/dangerous 规则对内部命令完全不评估——
    `rm -rf $(echo /)` 从 destructive 降级为 default。
    """

    def test_deny_layer_hit_with_command_substitution(self):
        """rm 的危险规则（first_arg_in -rf）在替换形态下仍然命中——
        修复前整体跳过、降级 default（可写沙箱）。"""
        assert _decision('bash -c "rm -rf $(echo /)"') == "dangerous"

    def test_destructive_segment_behind_substitution(self):
        """切段评估：rm -rf / 段干净命中 destructive（deny），
        同链后续段的 $(date) 不得拖垮整段扫描。"""
        assert _decision('bash -c "rm -rf /; echo $(date)"') == "destructive"

    def test_dangerous_chmod_with_substitution(self):
        assert _decision('bash -c "chmod 777 $(pwd)/shadow"') == "dangerous"

    def test_plain_destructive_still_works(self):
        """正常链路保护：无操作符的内联命令照旧评估。"""
        assert _decision('bash -c "rm -rf /"') == "destructive"

    def test_benign_wrapper_still_default(self):
        """正常链路保护：无害脚本仍不误报。"""
        assert _decision('bash -c "echo $(date)"') == "default"


class TestSedInPlaceClassification:
    """sed 就地编辑的各形态不得误判为只读（safe）。"""

    def test_in_place_long_flag_is_not_safe(self):
        """修复前：--in-place 不匹配 startswith("-i")，判"sed 只读模式"。"""
        assert _decision("sed --in-place 's/a/b/' file.txt") != "safe"

    def test_in_place_long_flag_with_suffix_is_not_safe(self):
        assert _decision("sed --in-place=.bak 's/a/b/' file.txt") != "safe"

    def test_bundled_short_flags_is_not_safe(self):
        """-ni（= -n -i）同样就地编辑。"""
        assert _decision("sed -ni 's/a/b/p' file.txt") != "safe"

    def test_in_place_with_suffix_is_dangerous(self):
        """修复前：危险规则 flags 精确匹配 ["-i"]，-i.bak 落到 default。"""
        assert _decision("sed -i.bak 's/a/b/' file.txt") == "dangerous"

    def test_long_flag_is_dangerous(self):
        assert _decision("sed --in-place 's/a/b/' file.txt") == "dangerous"

    def test_readonly_sed_still_safe(self):
        """正常链路保护：无就地标志仍为只读。"""
        assert _decision("sed 's/a/b/' file.txt") == "safe"

    def test_plain_in_place_still_dangerous(self):
        """正常链路保护：-i 照旧 dangerous。"""
        assert _decision("sed -i 's/a/b/' file.txt") == "dangerous"
