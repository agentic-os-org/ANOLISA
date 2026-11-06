"""
文件解析器 - 解析 Skill 文件结构
"""

import os
from pathlib import Path
from typing import List, Optional


class SkillParser:
    """解析 Skill 目录结构"""

    def __init__(self, skill_path: str):
        self.skill_path = Path(skill_path)

    def parse(self) -> dict:
        """
        解析 Skill 目录

        Returns:
            dict: 包含文件列表、元数据等信息
        """
        if not self.skill_path.exists():
            raise FileNotFoundError(f"Skill path not found: {self.skill_path}")

        result = {
            "path": str(self.skill_path),
            "files": [],
            "metadata": self._extract_metadata(),
        }

        # 收集所有相关文件
        for file_path in self._collect_files():
            file_info = {
                "path": str(file_path),
                "relative_path": str(file_path.relative_to(self.skill_path)),
                "size": file_path.stat().st_size,
                "extension": file_path.suffix,
            }
            result["files"].append(file_info)

        return result

    def _collect_files(self) -> List[Path]:
        """
        收集需要扫描的文件

        Returns:
            List[Path]: 文件路径列表
        """
        files = []

        # 定义需要扫描的文件扩展名
        scan_extensions = {
            ".md",
            ".txt",
            ".py",
            ".js",
            ".ts",
            ".sh",
            ".bash",
            ".yml",
            ".yaml",
            ".json",
            ".toml",
        }

        # 定义需要排除的目录
        exclude_dirs = {
            ".git",
            "__pycache__",
            "node_modules",
            ".venv",
            "venv",
            "dist",
            "build",
            ".pytest_cache",
        }

        if self.skill_path.is_file():
            return [self.skill_path]

        # 递归扫描目录
        for root, dirs, filenames in os.walk(self.skill_path):
            # 过滤排除的目录
            dirs[:] = [d for d in dirs if d not in exclude_dirs]

            for filename in filenames:
                file_path = Path(root) / filename

                # 检查文件扩展名或是否为 SKILL.md
                if filename == "SKILL.md" or file_path.suffix in scan_extensions:
                    files.append(file_path)

        return files

    def _extract_metadata(self) -> dict:
        """
        提取 Skill 元数据

        Returns:
            dict: 元数据信息
        """
        metadata = {
            "name": self.skill_path.name,
            "has_skill_md": False,
            "description": None,
        }

        skill_md = self.skill_path / "SKILL.md"
        if skill_md.exists():
            metadata["has_skill_md"] = True
            metadata["description"] = self._extract_description(skill_md)

        return metadata

    def _extract_description(self, skill_md_path: Path) -> Optional[str]:
        """
        从 SKILL.md 提取描述信息

        Args:
            skill_md_path: SKILL.md 文件路径

        Returns:
            Optional[str]: 描述文本
        """
        try:
            with open(skill_md_path, "r", encoding="utf-8") as f:
                lines = f.readlines()

                # 查找描述部分（通常在 frontmatter 后或标题下）
                for i, line in enumerate(lines):
                    if line.strip().startswith("description:"):
                        # 从 YAML frontmatter 提取
                        return line.split(":", 1)[1].strip()
                    elif line.lower().startswith(
                        "## description"
                    ) or line.lower().startswith("## 描述"):
                        # 从 markdown 标题后提取
                        desc_lines = []
                        for j in range(i + 1, min(i + 5, len(lines))):
                            if lines[j].strip() and not lines[j].startswith("#"):
                                desc_lines.append(lines[j].strip())
                            elif desc_lines:
                                break
                        return " ".join(desc_lines) if desc_lines else None

        except Exception as e:
            print(f"Warning: Could not read SKILL.md: {e}")

        return None
