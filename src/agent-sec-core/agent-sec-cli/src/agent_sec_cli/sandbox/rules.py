#!/usr/bin/env python3
"""
agent-sec-sandbox 安全规则配置
"""

# ============================================================================
# 毁灭性命令 - 直接 DENY
# ============================================================================
DESTRUCTIVE_RULES = [
    {"command": "fdisk", "reason": "磁盘分区"},
    {"command": "parted", "reason": "磁盘分区"},
    {"command": "gdisk", "reason": "磁盘分区"},
    {"command": "cfdisk", "reason": "磁盘分区"},
    {"command_prefix": "mkfs", "reason": "磁盘格式化"},
    {"command": "shutdown", "reason": "关机"},
    {"command": "reboot", "reason": "重启"},
    {"command": "poweroff", "reason": "关机"},
    {"command": "halt", "reason": "停机"},
    {"command": "init", "reason": "系统初始化"},
    {
        "command": "rm",
        "flags": ["-f", "-rf", "-fr"],
        "target_in": [
            "/",
            "/*",
            "/etc",
            "/usr",
            "/var",
            "/boot",
            "/bin",
            "/sbin",
            "/lib",
            "/lib64",
            "/root",
            "/home",
        ],
        "reason": "删除系统关键目录",
    },
    {
        "command": "dd",
        "args_contain": ["of=/dev/sd", "of=/dev/nvme", "of=/dev/hd", "of=/dev/vd"],
        "reason": "写入块设备",
    },
    {
        "command": "kill",
        "args_contain": ["-9 -1", "-KILL -1"],
        "reason": "杀死所有进程",
    },
    {"command": "kill", "target_in": ["1"], "reason": "杀死 init 进程"},
    {"pattern": ":(){ :|:& };:", "reason": "Fork 炸弹"},
    {"pattern": ":(){ :|:", "reason": "Fork 炸弹变种"},
]

# ============================================================================
# 危险命令 - 沙箱执行（cwd + /tmp 可写，禁止网络）
# ============================================================================
DANGEROUS_RULES = [
    {
        "command": "rm",
        "first_arg_in": ["-f", "-rf", "-fr", "-r"],
        "reason": "删除文件或目录",
    },
    {"command": "chmod", "reason": "修改文件权限"},
    {"command": "chown", "reason": "修改文件归属"},
    {
        "command": "find",
        "args_contain": ["-delete", "-exec", "-execdir", "-ok", "-okdir"],
        "reason": "find 执行删除或外部命令",
    },
    {"command": "sed", "flags": ["-i"], "reason": "sed 原地修改文件"},
    {"command": "git", "subcommands": ["clean"], "reason": "git clean 删除未跟踪文件"},
    {"command": "sudo", "reason": "提权执行"},
]


# ============================================================================
# 安全命令 - 只读模板（禁网）
# ============================================================================
SAFE_COMMANDS = frozenset(
    [
        "cat",
        "cd",
        "cut",
        "echo",
        "expr",
        "false",
        "grep",
        "head",
        "id",
        "ls",
        "nl",
        "paste",
        "pwd",
        "rev",
        "seq",
        "stat",
        "tail",
        "tr",
        "true",
        "uname",
        "uniq",
        "wc",
        "which",
        "whoami",
    ]
)

# 仅 Linux 的安全命令
SAFE_COMMANDS_LINUX = frozenset(["numfmt", "tac"])

# 条件安全命令
SAFE_CONDITIONAL = [
    {
        "command": "base64",
        "deny_args": ["-o", "--output", "--output="],
        "deny_arg_prefixes": ["-o"],  # -ob64.txt 形式
        "reason": "Base64 编解码（无输出重定向）",
    },
    {
        "command": "find",
        "deny_args": [
            "-exec",
            "-execdir",
            "-ok",
            "-okdir",
            "-delete",
            "-fls",
            "-fprint",
            "-fprint0",
            "-fprintf",
        ],
        "reason": "文件查找（不含执行/删除参数）",
    },
    {
        "command": "rg",
        "deny_args": [
            "--pre",
            "--pre=",
            "--hostname-bin",
            "--hostname-bin=",
            "--search-zip",
            "-z",
        ],
        "reason": "Ripgrep（不含外部命令调用）",
    },
    # git 和 sed 有 special_handler，在代码中单独处理
]

# Shell 不安全操作符（用于 bash -c 解析）
SHELL_UNSAFE_OPERATORS = frozenset([">", "<", ">>", "(", ")", "`", "$("])
SHELL_SAFE_OPERATORS = frozenset(["&&", "||", ";", "|"])

# ============================================================================
# 额外权限配置 - default 命令的网络/路径权限
# ============================================================================
PERMISSION_RULES = [
    # 网络 + 额外写路径（更具体的规则放前面，优先匹配）
    {
        "command": "npm",
        "subcommands": ["install"],
        "args_contain": ["-g", "--global"],
        "grant": {
            "network": True,
            "write_paths": ["/usr/local/lib/node_modules", "/usr/local/bin"],
        },
    },
    {
        "command": "pip",
        "subcommands": ["install"],
        "args_contain": ["--system"],
        "grant": {
            "network": True,
            "write_paths": ["/usr/lib/python3", "/usr/local/lib/python3"],
        },
    },
    {
        "command": "docker",
        "subcommands": ["build", "run", "compose"],
        "grant": {"network": True, "write_paths": ["/var/lib/docker"]},
    },
    # 网络权限（通用规则）
    {
        "command": "cargo",
        "subcommands": ["build", "run", "test", "fetch", "update", "install", "add"],
        "grant": {"network": True},
    },
    {
        "command": "npm",
        "subcommands": ["install", "ci", "update", "audit"],
        "grant": {"network": True},
    },
    {
        "command": "yarn",
        "subcommands": ["install", "add", "upgrade"],
        "grant": {"network": True},
    },
    {
        "command": "pnpm",
        "subcommands": ["install", "add", "update"],
        "grant": {"network": True},
    },
    {
        "command": "pip",
        "subcommands": ["install", "download"],
        "grant": {"network": True},
    },
    {
        "command": "pip3",
        "subcommands": ["install", "download"],
        "grant": {"network": True},
    },
    {
        "command": "go",
        "subcommands": ["get", "mod", "install"],
        "grant": {"network": True},
    },
    {
        "command": "gem",
        "subcommands": ["install", "update"],
        "grant": {"network": True},
    },
    {
        "command": "bundle",
        "subcommands": ["install", "update"],
        "grant": {"network": True},
    },
    {"command": "curl", "grant": {"network": True}},
    {"command": "wget", "grant": {"network": True}},
    {
        "command": "git",
        "subcommands": ["clone", "fetch", "pull", "push"],
        "grant": {"network": True},
    },
]
