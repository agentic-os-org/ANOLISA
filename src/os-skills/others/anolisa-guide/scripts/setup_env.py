#!/usr/bin/env python3
"""
setup_env.py - 创建独立的 Python 虚拟环境并安装依赖

环境位置: ~/.cache/anolisa/.venv/
安装依赖: requests, beautifulsoup4, markdownify

使用方式:
    python3 setup_env.py                    # 创建虚拟环境
    python3 setup_env.py --check            # 检查环境状态
    python3 setup_env.py --force            # 强制重建

SKILL.md 指导:
    在执行 check_docs.py 前，AI 应先调用此脚本确保环境就绪
"""

import subprocess
import sys
import shutil
from pathlib import Path


# 用户缓存目录
USER_CACHE_DIR = Path.home() / ".cache" / "anolisa"

# 文档缓存目录
CACHE_DIR = USER_CACHE_DIR / "skills" / "anolisa-guide" / "reference"

# 虚拟环境目录
VENV_DIR = USER_CACHE_DIR / ".venv"

# 需要安装的依赖
DEPENDENCIES = ["requests", "beautifulsoup4", "markdownify"]


def check_venv() -> bool:
    """检查虚拟环境是否存在且依赖已安装"""
    if not VENV_DIR.exists():
        return False
    
    venv_python = VENV_DIR / "bin" / "python"
    if not venv_python.exists():
        return False
    
    # 尝试导入依赖
    try:
        result = subprocess.run(
            [str(venv_python), "-c", 
             "import requests, bs4, markdownify"],
            capture_output=True,
            timeout=5
        )
        return result.returncode == 0
    except Exception:
        return False


def create_venv() -> bool:
    """创建虚拟环境"""
    CACHE_DIR.mkdir(parents=True, exist_ok=True)
    
    result = subprocess.run(
        [sys.executable, "-m", "venv", str(VENV_DIR)],
        capture_output=True
    )
    
    return result.returncode == 0


def install_dependencies() -> bool:
    """安装依赖"""
    venv_pip = VENV_DIR / "bin" / "pip"
    
    if not venv_pip.exists():
        return False
    
    result = subprocess.run(
        [str(venv_pip), "install", "--quiet", "--disable-pip-version-check"] + DEPENDENCIES,
        capture_output=True,
        timeout=120
    )
    
    return result.returncode == 0


def get_venv_python() -> Path:
    """获取虚拟环境的 Python 路径"""
    return VENV_DIR / "bin" / "python"


def main():
    import argparse
    
    parser = argparse.ArgumentParser(description="创建虚拟环境")
    parser.add_argument("--check", action="store_true", help="仅检查状态")
    parser.add_argument("--force", action="store_true", help="强制重建")
    
    args = parser.parse_args()
    
    # 仅检查
    if args.check:
        if check_venv():
            print(str(get_venv_python()))
            sys.exit(0)
        else:
            print("NOT_READY")
            sys.exit(1)
    
    # 强制重建
    if args.force and VENV_DIR.exists():
        shutil.rmtree(VENV_DIR)
    
    # 检查是否已就绪
    if check_venv():
        print(f"[跳过] 虚拟环境已就绪: {get_venv_python()}")
        sys.exit(0)
    
    # 创建虚拟环境
    print(f"[创建] {VENV_DIR}")
    if not create_venv():
        print("[失败] 创建虚拟环境失败")
        sys.exit(1)
    
    # 安装依赖
    print(f"[安装] {', '.join(DEPENDENCIES)}")
    if not install_dependencies():
        print("[失败] 安装依赖失败")
        sys.exit(1)
    
    # 输出 Python 路径供后续使用
    print(f"[完成] {get_venv_python()}")
    sys.exit(0)


if __name__ == "__main__":
    main()
