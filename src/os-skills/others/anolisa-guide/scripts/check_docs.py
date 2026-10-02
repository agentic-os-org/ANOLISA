#!/usr/bin/env python3
"""
check_docs.py - ANOLISA 文档时效性检查和选择脚本

功能：
1. 检查静态文档时效性（优先使用）
2. 静态文档过期时，检查用户缓存目录
3. 如需要更新，自动创建虚拟环境并爬取
4. 选择应该使用的文档目录

使用：
    python3 check_docs.py
    
输出：
    返回应该使用的文档目录路径

文档选择优先级：
    静态文档（新鲜） > 缓存文档（新鲜） > 缓存文档（更新后） > 静态文档（兜底）
"""

import os
import sys
import subprocess
from datetime import datetime, timedelta
from pathlib import Path


# 配置
MAX_DAYS = 7  # 最大允许天数
TIMEOUT = 180  # 爬取超时时间（秒）

# 目录常量
USER_CACHE_DIR = Path.home() / ".cache" / "anolisa"
CACHE_DIR = USER_CACHE_DIR / "skills" / "anolisa-guide" / "reference"
STATIC_DIR = Path("/usr/share/anolisa/skills/anolisa-guide/reference")
VENV_DIR = USER_CACHE_DIR / ".venv"

# 脚本目录
STATIC_SCRIPT_DIR = Path("/usr/share/anolisa/skills/anolisa-guide/scripts")
CURRENT_SCRIPT_DIR = Path(__file__).parent.resolve()

if STATIC_SCRIPT_DIR.exists() and (STATIC_SCRIPT_DIR / "setup_env.py").exists():
    SCRIPT_DIR = STATIC_SCRIPT_DIR
else:
    SCRIPT_DIR = CURRENT_SCRIPT_DIR


def get_crawl_time(filepath: Path) -> datetime | None:
    """从文件中提取爬取时间"""
    try:
        content = filepath.read_text(encoding='utf-8')
        for line in content.split('\n')[:10]:
            if '**爬取时间**:' in line or '爬取:' in line:
                if '**爬取时间**:' in line:
                    parts = line.split('**爬取时间**:')
                else:
                    parts = line.split('爬取:')
                
                if len(parts) >= 2:
                    time_str = parts[1].strip().split('|')[0].strip()
                    try:
                        return datetime.strptime(time_str, "%Y-%m-%d %H:%M:%S")
                    except ValueError:
                        try:
                            return datetime.fromisoformat(time_str)
                        except:
                            pass
        return None
    except Exception:
        return None


def check_freshness(directory: Path) -> tuple[bool | None, str]:
    """检查目录中文档的时效性"""
    if not directory.exists():
        return None, "目录不存在"
    
    md_files = list(directory.glob("*.md"))
    if len(md_files) < 13:
        return None, f"文档不完整（{len(md_files)}/13）"
    
    now = datetime.now()
    threshold = now - timedelta(days=MAX_DAYS)
    
    newest_time = None
    for md_file in md_files:
        crawl_time = get_crawl_time(md_file)
        if crawl_time is None:
            continue
        if newest_time is None or crawl_time > newest_time:
            newest_time = crawl_time
    
    if newest_time is None:
        return None, "无法解析时间戳"
    
    if newest_time < threshold:
        days_old = (now - newest_time).days
        return False, f"文档过期（{newest_time.strftime('%Y-%m-%d')}，已 {days_old} 天）"
    
    return True, f"文档时效性良好（{newest_time.strftime('%Y-%m-%d')}）"


def get_venv_python() -> Path:
    """获取虚拟环境的 Python 路径"""
    return VENV_DIR / "bin" / "python"


def check_venv() -> bool:
    """检查虚拟环境是否存在且依赖已安装"""
    venv_python = get_venv_python()
    if not venv_python.exists():
        return False
    
    try:
        result = subprocess.run(
            [str(venv_python), "-c", "import requests, bs4, markdownify"],
            capture_output=True,
            timeout=5
        )
        return result.returncode == 0
    except Exception:
        return False


def ensure_venv() -> Path | None:
    """确保虚拟环境存在，不存在则自动创建"""
    if check_venv():
        print("[虚拟环境] 已就绪，直接复用")
        return get_venv_python()
    
    setup_script = SCRIPT_DIR / "setup_env.py"
    
    if not setup_script.exists():
        print(f"[错误] setup_env.py 不存在: {setup_script}")
        return None
    
    print("[虚拟环境] 正在自动创建并安装依赖...")
    
    try:
        result = subprocess.run(
            [sys.executable, str(setup_script)],
            cwd=SCRIPT_DIR,
            capture_output=True,
            text=True,
            timeout=120
        )
        
        if result.returncode == 0 and check_venv():
            print("[虚拟环境] 创建成功，后续可直接复用")
            return get_venv_python()
        else:
            print(f"[失败] {result.stderr}")
            return None
            
    except Exception as e:
        print(f"[异常] {e}")
        return None


def run_crawl(output_dir: Path) -> bool:
    """爬取更新文档（自动使用虚拟环境）"""
    # 自动确保虚拟环境
    venv_python = ensure_venv()
    if venv_python is None:
        print("[错误] 无法创建虚拟环境")
        return False
    
    crawl_script = SCRIPT_DIR / "crawl_docs.py"
    
    if not crawl_script.exists():
        print(f"[错误] crawl_docs.py 不存在: {crawl_script}")
        return False
    
    try:
        print(f"[爬取] 正在更新文档到: {output_dir}")
        
        result = subprocess.run(
            [str(venv_python), str(crawl_script), "--output-dir", str(output_dir)],
            cwd=SCRIPT_DIR,
            capture_output=True,
            text=True,
            timeout=TIMEOUT
        )
        
        if result.returncode == 0:
            print("[成功] 文档已更新")
            return True
        else:
            print(f"[失败] {result.stderr}")
            return False
            
    except subprocess.TimeoutExpired:
        print("[超时] 爬取超时")
        return False
    except Exception as e:
        print(f"[异常] {e}")
        return False


def main():
    """主函数：检查并选择文档目录（优先静态文档）"""
    
    # 1. 优先检查静态文档
    if STATIC_DIR.exists():
        fresh, msg = check_freshness(STATIC_DIR)
        
        if fresh:
            # 静态文档新鲜，直接使用
            print(f"[使用静态] {msg}")
            print(STATIC_DIR)
            return 0
        
        # 静态文档过期，进入更新流程
        print(f"[静态过期] {msg}")
    
    # 2. 检查用户缓存目录
    if CACHE_DIR.exists():
        fresh, msg = check_freshness(CACHE_DIR)
        
        if fresh:
            # 缓存文档新鲜，使用缓存
            print(f"[使用缓存] {msg}")
            print(CACHE_DIR)
            return 0
        
        # 缓存过期，更新缓存
        print(f"[缓存过期] {msg}")
        if run_crawl(CACHE_DIR):
            print(CACHE_DIR)
            return 0
    
    # 3. 需要创建缓存并更新
    print("[创建缓存] 正在准备文档...")
    CACHE_DIR.mkdir(parents=True, exist_ok=True)
    
    if run_crawl(CACHE_DIR):
        print(CACHE_DIR)
        return 0
    
    # 4. 爬取失败，使用静态文档兜底
    if STATIC_DIR.exists():
        print("[兜底] 使用静态文档")
        print(STATIC_DIR)
        return 0
    
    # 5. 完全失败
    print("[错误] 无法获取文档")
    return 1


if __name__ == "__main__":
    sys.exit(main())