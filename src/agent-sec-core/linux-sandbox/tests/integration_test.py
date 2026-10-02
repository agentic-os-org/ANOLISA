#!/usr/bin/env python3
"""linux-sandbox 全面测试脚本

测试所有策略类型和安全特性:
- 只读文件系统策略
- 可写工作区策略
- 完全不受限策略
- 网络限制
- 敏感目录保护
- 系统调用过滤 (seccomp)
- 进程隔离
"""

import ctypes
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

# 颜色输出
RED = "\033[0;31m"
GREEN = "\033[0;32m"
YELLOW = "\033[1;33m"
BLUE = "\033[0;34m"
NC = "\033[0m"

SANDBOX = shutil.which("linux-sandbox") or "/usr/local/bin/linux-sandbox"
# 是否显示详细输出 (通过 -v 或 --verbose 参数启用)
VERBOSE = False


@dataclass
class TestResult:
    passed: int = 0
    failed: int = 0


results = TestResult()


# ==============================================================================
# 新策略帮助函数
# ==============================================================================


def fs_read_only():
    """创建只读文件系统策略"""
    return {
        "kind": "restricted",
        "entries": [
            {"path": {"type": "special", "value": {"kind": "root"}}, "access": "read"}
        ],
    }


def fs_unrestricted():
    """创建完全不受限文件系统策略"""
    return {"kind": "unrestricted", "entries": []}


def fs_workspace_write(writable_roots: list, include_tmp=True, include_tmpdir=True):
    """创建可写工作区文件系统策略"""
    entries = [
        {"path": {"type": "special", "value": {"kind": "root"}}, "access": "read"},
        {
            "path": {"type": "special", "value": {"kind": "current_working_directory"}},
            "access": "write",
        },
    ]
    for root in writable_roots:
        entries.append({"path": {"type": "path", "path": root}, "access": "write"})
    if include_tmp:
        entries.append(
            {
                "path": {"type": "special", "value": {"kind": "slash_tmp"}},
                "access": "write",
            }
        )
    if include_tmpdir:
        entries.append(
            {
                "path": {"type": "special", "value": {"kind": "tmpdir"}},
                "access": "write",
            }
        )
    return {"kind": "restricted", "entries": entries}


def fs_restricted_read(readable_roots: list):
    """创建受限读取策略

    始终包含根目录只读 entry，确保系统程序（cat/bash 等）及其依赖库可以执行。
    在此基础上叠加调用方指定的可读路径。
    """
    entries = [
        {"path": {"type": "special", "value": {"kind": "root"}}, "access": "read"},
    ]
    for root in readable_roots:
        entries.append({"path": {"type": "path", "path": root}, "access": "read"})
    return {"kind": "restricted", "entries": entries}


def run_sandbox(
    fs_policy: dict,
    net_policy: str,
    command: list[str],
    cwd: Optional[str] = None,
    check: bool = False,
) -> subprocess.CompletedProcess:
    """运行沙箱命令"""
    if cwd is None:
        cwd = os.getcwd()

    cmd = [
        SANDBOX,
        "--sandbox-policy-cwd",
        cwd,
        "--file-system-sandbox-policy",
        json.dumps(fs_policy),
        "--network-sandbox-policy",
        json.dumps(net_policy),
        "--",
    ] + command

    if VERBOSE:
        print(f"  命令: {' '.join(cmd[:8])} ... {' '.join(command)}")

    result = subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        check=check,
    )

    if VERBOSE:
        if result.stdout.strip():
            print(f"  stdout: {result.stdout.strip()[:200]}")
        if result.stderr.strip():
            print(f"  stderr: {result.stderr.strip()[:200]}")
        print(f"  exit: {result.returncode}")

    return result


def test(
    name: str,
    expect_success: bool,
    fs_policy: dict,
    net_policy: str,
    command: list[str],
    cwd: Optional[str] = None,
    expect_stdout: Optional[str] = None,
    expect_stderr_contains: Optional[list[str]] = None,
) -> bool:
    """运行单个测试

    Args:
        name: 测试名称
        expect_success: 是否期望成功 (exit code = 0)
        fs_policy: 文件系统沙箱策略
        net_policy: 网络沙箱策略 ("restricted" 或 "enabled")
        command: 要执行的命令
        cwd: 工作目录
        expect_stdout: 期望 stdout 包含的内容
        expect_stderr_contains: 期望 stderr 包含的内容列表 (任一匹配即可)
    """
    print(f"\n{BLUE}--- {name} ---{NC}")

    result = run_sandbox(fs_policy, net_policy, command, cwd)
    actual_success = result.returncode == 0

    # 基本验证: exit code
    if actual_success != expect_success:
        print(
            f"{RED}✗ 失败{NC} (exit code: 期望 {'0' if expect_success else '非0'}, 实际: {result.returncode})"
        )
        print(f"  stdout: {result.stdout[:300]}")
        print(f"  stderr: {result.stderr[:300]}")
        results.failed += 1
        return False

    # 验证 stdout 内容
    if expect_stdout and expect_stdout not in result.stdout:
        print(f"{RED}✗ 失败{NC} (stdout 未包含期望内容: '{expect_stdout}')")
        print(f"  实际 stdout: {result.stdout[:300]}")
        results.failed += 1
        return False

    # 验证 stderr 内容 (对于失败的测试，验证是否是正确的错误原因)
    if expect_stderr_contains:
        stderr_lower = result.stderr.lower() + result.stdout.lower()
        if not any(err.lower() in stderr_lower for err in expect_stderr_contains):
            print(f"{RED}✗ 失败{NC} (stderr 未包含期望错误: {expect_stderr_contains})")
            print(f"  实际 stderr: {result.stderr[:300]}")
            print(f"  实际 stdout: {result.stdout[:300]}")
            results.failed += 1
            return False

    print(f"{GREEN}✓ 通过{NC} (exit: {result.returncode})")
    results.passed += 1
    return True


def test_python_code(
    name: str,
    expect_success: bool,
    fs_policy: dict,
    net_policy: str,
    code: str,
    cwd: Optional[str] = None,
    expect_stderr_contains: Optional[list[str]] = None,
) -> bool:
    """运行 Python 代码测试"""
    return test(
        name,
        expect_success,
        fs_policy,
        net_policy,
        ["python3", "-c", code],
        cwd,
        expect_stderr_contains=expect_stderr_contains,
    )


def test_shell(
    name: str,
    expect_success: bool,
    fs_policy: dict,
    net_policy: str,
    shell_cmd: str,
    cwd: Optional[str] = None,
    expect_stderr_contains: Optional[list[str]] = None,
) -> bool:
    """运行 shell 命令测试"""
    return test(
        name,
        expect_success,
        fs_policy,
        net_policy,
        ["bash", "-c", shell_cmd],
        cwd,
        expect_stderr_contains=expect_stderr_contains,
    )


def verify_file(path: str, expected_content: str) -> bool:
    """验证文件内容"""
    try:
        with open(path) as f:
            content = f.read().strip()
        if content == expected_content:
            print(f"{GREEN}✓ 文件验证通过: {path}{NC}")
            results.passed += 1
            return True
        else:
            print(
                f"{RED}✗ 文件验证失败: 期望 '{expected_content}', 实际 '{content}'{NC}"
            )
            results.failed += 1
            return False
    except Exception as e:
        print(f"{RED}✗ 文件验证失败: {e}{NC}")
        results.failed += 1
        return False


def main():
    # 检查沙箱是否存在
    if not os.path.isfile(SANDBOX):
        print(f"{RED}错误: 找不到 {SANDBOX}{NC}")
        print(
            "请先编译并安装: cargo build --release && sudo cp target/release/linux-sandbox"
            " 到 PATH 中的目录 (如 /usr/local/bin/ 或 ~/.local/bin/)"
        )
        sys.exit(1)

    cwd = os.getcwd()

    # 创建测试临时目录
    test_base = tempfile.mkdtemp(prefix="sandbox_test_")

    try:
        print("=" * 50)
        print("linux-sandbox 全面测试")
        print("=" * 50)

        # ================================================================
        # 测试组 1: 只读文件系统策略
        # ================================================================
        print(f"\n{YELLOW}### 测试组 1: 只读文件系统策略 ###{NC}")

        # 基本读取 - 验证确实读到内容
        test(
            "只读: 读取根目录",
            True,
            fs_read_only(),
            "restricted",
            ["ls", "/"],
            cwd,
            expect_stdout="bin",
        )  # 根目录必须包含 bin
        test(
            "只读: 读取 /etc/passwd",
            True,
            fs_read_only(),
            "restricted",
            ["cat", "/etc/passwd"],
            cwd,
            expect_stdout="root:",
        )  # passwd 必须包含 root 用户

        # 写入应失败 - 验证是正确的错误原因
        FS_DENY_ERRORS = ["Read-only file system", "Permission denied", "No such file"]
        test_shell(
            "只读: 写入根目录失败",
            False,
            fs_read_only(),
            "restricted",
            "echo x > /testfile",
            cwd,
            expect_stderr_contains=FS_DENY_ERRORS,
        )
        test_shell(
            "只读: 写入 /tmp 失败",
            False,
            fs_read_only(),
            "restricted",
            "echo x > /tmp/test_ro",
            cwd,
            expect_stderr_contains=FS_DENY_ERRORS,
        )
        test_shell(
            "只读: 创建目录失败",
            False,
            fs_read_only(),
            "restricted",
            "mkdir /tmp/newdir_ro",
            cwd,
            expect_stderr_contains=FS_DENY_ERRORS,
        )

        # 网络控制 - 验证是网络相关错误
        NET_DENY_ERRORS = [
            "Operation not permitted",
            "Permission denied",
            "EPERM",
            "Network is unreachable",
        ]
        test_python_code(
            "只读: 网络受限时阻止网络",
            False,
            fs_read_only(),
            "restricted",
            "import socket; s=socket.socket(socket.AF_INET, socket.SOCK_STREAM); s.connect(('8.8.8.8', 53))",
            cwd,
            expect_stderr_contains=NET_DENY_ERRORS,
        )

        test_python_code(
            "只读: 网络启用时允许创建 socket",
            True,
            fs_read_only(),
            "enabled",
            "import socket; s=socket.socket(socket.AF_INET, socket.SOCK_STREAM); s.close()",
            cwd,
        )

        # 限制读取范围
        restricted_dir = os.path.join(test_base, "restricted_read")
        os.makedirs(restricted_dir)
        with open(os.path.join(restricted_dir, "data.txt"), "w") as f:
            f.write("secret")

        readonly_restricted = fs_restricted_read([restricted_dir])

        test(
            "只读受限: 读取允许的目录",
            True,
            readonly_restricted,
            "restricted",
            ["cat", os.path.join(restricted_dir, "data.txt")],
            restricted_dir,
        )

        # ================================================================
        # 测试组 2: 可写工作区策略
        # ================================================================
        print(f"\n{YELLOW}### 测试组 2: 可写工作区策略 ###{NC}")

        workspace = os.path.join(test_base, "workspace")
        output = os.path.join(test_base, "output")
        os.makedirs(workspace)
        os.makedirs(output)

        # 基本写入
        test_file = os.path.join(workspace, "test.txt")
        test_shell(
            "可写工作区: 写入 cwd",
            True,
            fs_workspace_write([workspace]),
            "restricted",
            f"echo hello > {test_file}",
            workspace,
        )
        verify_file(test_file, "hello")

        test_shell(
            "可写工作区: 创建子目录",
            True,
            fs_workspace_write([workspace]),
            "restricted",
            f"mkdir -p {workspace}/sub/dir && echo x > {workspace}/sub/dir/file.txt",
            workspace,
        )

        # 多个可写目录
        test_shell(
            "可写工作区: 写入多个可写目录",
            True,
            fs_workspace_write([workspace, output]),
            "restricted",
            f"echo a > {workspace}/a.txt && echo b > {output}/b.txt",
            workspace,
        )

        # 写入非可写目录失败
        test_shell(
            "可写工作区: 写入非可写目录失败",
            False,
            fs_workspace_write([workspace]),
            "restricted",
            "echo x > /etc/test",
            workspace,
            expect_stderr_contains=FS_DENY_ERRORS,
        )

        # /tmp 控制
        test_shell(
            "可写工作区: /tmp 默认可写",
            True,
            fs_workspace_write([workspace], include_tmp=True),
            "restricted",
            f"echo x > /tmp/test_tmp_allow_{os.getpid()}",
            workspace,
        )

        test_shell(
            "可写工作区: include_tmp=False 阻止 /tmp",
            False,
            fs_workspace_write([workspace], include_tmp=False),
            "restricted",
            f"echo x > /tmp/test_tmp_deny_{os.getpid()}",
            workspace,
            expect_stderr_contains=FS_DENY_ERRORS,
        )

        # $TMPDIR 控制
        tmpdir = os.path.join(test_base, "tmpdir")
        os.makedirs(tmpdir)
        os.environ["TMPDIR"] = tmpdir

        test_shell(
            "可写工作区: $TMPDIR 默认可写",
            True,
            fs_workspace_write([workspace], include_tmp=False, include_tmpdir=True),
            "restricted",
            f"echo x > {tmpdir}/test_tmpdir_allow",
            workspace,
        )

        test_shell(
            "可写工作区: include_tmpdir=False 阻止 $TMPDIR",
            False,
            fs_workspace_write([workspace], include_tmp=False, include_tmpdir=False),
            "restricted",
            f"echo x > {tmpdir}/test_tmpdir_deny",
            workspace,
            expect_stderr_contains=FS_DENY_ERRORS,
        )

        # 网络控制
        test_python_code(
            "可写工作区: network=enabled 允许网络",
            True,
            fs_workspace_write([workspace]),
            "enabled",
            "import socket; s=socket.socket(socket.AF_INET, socket.SOCK_STREAM); s.close()",
            workspace,
        )

        test_python_code(
            "可写工作区: network=restricted 阻止网络",
            False,
            fs_workspace_write([workspace]),
            "restricted",
            "import socket; s=socket.socket(socket.AF_INET, socket.SOCK_STREAM); s.connect(('8.8.8.8', 53))",
            workspace,
            expect_stderr_contains=NET_DENY_ERRORS,
        )

        # ================================================================
        # 测试组 3: 敏感目录保护
        # ================================================================
        print(f"\n{YELLOW}### 测试组 3: 敏感目录保护 ###{NC}")

        protected = os.path.join(test_base, "protected")
        for subdir in [".git", ".copilot-shell", ".agents"]:
            os.makedirs(os.path.join(protected, subdir))
            with open(os.path.join(protected, subdir, "config"), "w") as f:
                f.write(f"{subdir}-config")

        fs_protected = fs_workspace_write(
            [protected], include_tmp=False, include_tmpdir=False
        )

        # 写入保护目录应失败
        test_shell(
            "敏感目录保护: .git 写入失败",
            False,
            fs_protected,
            "restricted",
            f"echo attack > {protected}/.git/config",
            protected,
            expect_stderr_contains=FS_DENY_ERRORS,
        )

        test_shell(
            "敏感目录保护: .copilot-shell 写入失败",
            False,
            fs_protected,
            "restricted",
            f"echo attack > {protected}/.copilot-shell/config",
            protected,
            expect_stderr_contains=FS_DENY_ERRORS,
        )

        test_shell(
            "敏感目录保护: .agents 写入失败",
            False,
            fs_protected,
            "restricted",
            f"echo attack > {protected}/.agents/config",
            protected,
            expect_stderr_contains=FS_DENY_ERRORS,
        )

        # 读取应成功 - 验证读到原始内容
        test(
            "敏感目录保护: .git 读取成功",
            True,
            fs_protected,
            "restricted",
            ["cat", os.path.join(protected, ".git/config")],
            protected,
            expect_stdout=".git-config",  # 验证读到的是原始内容
        )

        # 普通文件写入应成功
        test_shell(
            "敏感目录保护: 普通文件写入成功",
            True,
            fs_protected,
            "restricted",
            f"echo normal > {protected}/normal.txt",
            protected,
        )

        # ================================================================
        # 测试组 4: 不受限策略 (unrestricted)
        # ================================================================
        print(f"\n{YELLOW}### 测试组 4: 不受限策略 ###{NC}")

        unrestricted_dir = os.path.join(test_base, "unrestricted_test")
        os.makedirs(unrestricted_dir)

        test(
            "不受限: 读取文件",
            True,
            fs_unrestricted(),
            "restricted",
            ["cat", "/etc/passwd"],
            cwd,
        )

        test_shell(
            "不受限: 写入测试目录",
            True,
            fs_unrestricted(),
            "restricted",
            f"echo unrestricted > {unrestricted_dir}/test.txt",
            unrestricted_dir,
        )

        test_python_code(
            "不受限: 网络 restricted 时阻止网络",
            False,
            fs_unrestricted(),
            "restricted",
            "import socket; s=socket.socket(socket.AF_INET, socket.SOCK_STREAM); s.connect(('8.8.8.8', 53))",
            cwd,
            expect_stderr_contains=NET_DENY_ERRORS,
        )

        test_python_code(
            "不受限: 网络 enabled 时允许 socket",
            True,
            fs_unrestricted(),
            "enabled",
            "import socket; s=socket.socket(socket.AF_INET, socket.SOCK_STREAM); s.close()",
            cwd,
        )

        # ================================================================
        # 测试组 5: 系统调用过滤 (seccomp)
        # ================================================================
        print(f"\n{YELLOW}### 测试组 5: 系统调用过滤 (seccomp) ###{NC}")

        # seccomp 阻止的错误特征
        SECCOMP_DENY_ERRORS = ["Operation not permitted", "EPERM", "Permission denied"]

        # ptrace 被阻止 (seccomp 通过 SIGKILL 杀死进程，无 stderr 输出)
        test_python_code(
            "seccomp: ptrace 被阻止",
            False,
            fs_read_only(),
            "restricted",
            """
import ctypes
import sys
libc = ctypes.CDLL(None)
r = libc.ptrace(0, 0, 0, 0)  # PTRACE_TRACEME
sys.exit(0 if r == 0 else 1)
""",
            cwd,
        )

        # io_uring 被阻止
        test_python_code(
            "seccomp: io_uring_setup 被阻止",
            False,
            fs_read_only(),
            "restricted",
            """
import ctypes
import sys
libc = ctypes.CDLL(None)
r = libc.syscall(425, 1, 0)  # io_uring_setup
sys.exit(0 if r >= 0 else 1)
""",
            cwd,
        )

        test_python_code(
            "seccomp: io_uring_enter 被阻止",
            False,
            fs_read_only(),
            "restricted",
            """
import ctypes
import sys
libc = ctypes.CDLL(None)
r = libc.syscall(426, 0, 0, 0, 0, 0, 0)  # io_uring_enter
sys.exit(0 if r >= 0 else 1)
""",
            cwd,
        )

        test_python_code(
            "seccomp: io_uring_register 被阻止",
            False,
            fs_read_only(),
            "restricted",
            """
import ctypes
import sys
libc = ctypes.CDLL(None)
r = libc.syscall(427, 0, 0, 0, 0)  # io_uring_register
sys.exit(0 if r >= 0 else 1)
""",
            cwd,
        )

        # bind 被阻止（无网络时）
        test_python_code(
            "seccomp: bind 被阻止 (network=restricted)",
            False,
            fs_read_only(),
            "restricted",
            """
import socket
s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
s.bind(('127.0.0.1', 0))
""",
            cwd,
            expect_stderr_contains=NET_DENY_ERRORS,
        )

        # AF_UNIX 始终允许
        test_python_code(
            "seccomp: AF_UNIX socket 允许",
            True,
            fs_read_only(),
            "restricted",
            "import socket; s=socket.socket(socket.AF_UNIX, socket.SOCK_STREAM); s.close()",
            cwd,
        )

        # 普通系统调用允许
        test_python_code(
            "seccomp: getpid 允许",
            True,
            fs_read_only(),
            "restricted",
            "import os; print(os.getpid())",
            cwd,
        )

        test_python_code(
            "seccomp: 文件读取允许",
            True,
            fs_read_only(),
            "restricted",
            "f=open('/etc/hostname'); f.read(); f.close()",
            cwd,
        )

        # ================================================================
        # 测试组 6: 进程隔离
        # ================================================================
        print(f"\n{YELLOW}### 测试组 6: 进程隔离 ###{NC}")

        # PID namespace 隔离
        test_python_code(
            "进程隔离: PID namespace (PID < 10)",
            True,
            fs_read_only(),
            "restricted",
            """
import os
pid = os.getpid()
print(f'PID: {pid}')
assert pid < 10, f'PID {pid} should be small in PID namespace'
""",
            cwd,
        )

        # NO_NEW_PRIVS
        test_shell(
            "进程隔离: NO_NEW_PRIVS 启用",
            True,
            fs_read_only(),
            "restricted",
            'grep -q "NoNewPrivs:.*1" /proc/self/status',
            cwd,
        )

        # /dev 设备
        test_shell(
            "进程隔离: /dev/null 可写",
            True,
            fs_read_only(),
            "restricted",
            "echo test > /dev/null",
            cwd,
        )

        test_python_code(
            "进程隔离: /dev/urandom 可读",
            True,
            fs_read_only(),
            "restricted",
            "f=open('/dev/urandom','rb'); print(len(f.read(16))); f.close()",
            cwd,
        )

        # ================================================================
        # 测试组 7: 边界情况
        # ================================================================
        print(f"\n{YELLOW}### 测试组 7: 边界情况 ###{NC}")

        # 空 writable_roots（只有 cwd 可写）
        empty_wr = os.path.join(test_base, "empty_wr")
        os.makedirs(empty_wr)

        test_shell(
            "边界: 空 writable_roots，cwd 可写",
            True,
            fs_workspace_write([], include_tmp=False, include_tmpdir=False),
            "restricted",
            f"echo x > {empty_wr}/test.txt",
            empty_wr,
        )

        test_shell(
            "边界: 空 writable_roots，其他不可写",
            False,
            fs_workspace_write([], include_tmp=False, include_tmpdir=False),
            "restricted",
            "echo x > /tmp/test_empty_wr",
            empty_wr,
            expect_stderr_contains=FS_DENY_ERRORS,
        )

        # 嵌套可写目录
        nested = os.path.join(test_base, "nested")
        nested_sub = os.path.join(nested, "sub")
        os.makedirs(nested_sub)

        test_shell(
            "边界: 嵌套目录-子目录可写",
            True,
            fs_workspace_write([nested_sub], include_tmp=False, include_tmpdir=False),
            "restricted",
            f"echo x > {nested_sub}/test.txt",
            nested,
        )

        # 特殊字符路径
        special = os.path.join(test_base, "special dir")
        os.makedirs(special)

        test_shell(
            "边界: 路径包含空格",
            True,
            fs_workspace_write([special], include_tmp=False, include_tmpdir=False),
            "restricted",
            f'echo x > "{special}/test.txt"',
            special,
        )

    finally:
        # 清理
        shutil.rmtree(test_base, ignore_errors=True)
        for f in Path("/tmp").glob("test_tmp_*"):
            f.unlink(missing_ok=True)

    # ================================================================
    # 测试摘要
    # ================================================================
    print()
    print("=" * 50)
    print(f"{YELLOW}测试摘要{NC}")
    print("=" * 50)
    print(f"{GREEN}通过: {results.passed}{NC}")
    print(f"{RED}失败: {results.failed}{NC}")
    total = results.passed + results.failed
    print(f"总计: {total}")
    print("=" * 50)

    if results.failed == 0:
        print(f"{GREEN}所有测试通过!{NC}")
        sys.exit(0)
    else:
        print(f"{RED}有 {results.failed} 个测试失败!{NC}")
        sys.exit(1)


if __name__ == "__main__":
    import argparse

    parser = argparse.ArgumentParser(description="linux-sandbox 全面测试")
    parser.add_argument("-v", "--verbose", action="store_true", help="显示详细输出")
    args = parser.parse_args()
    VERBOSE = args.verbose
    main()
