# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.
"""公共命令行：只读取明确提供的数据，所有敏感信息检查先于输出。"""

import argparse
import os
import sys
import tempfile
import traceback
from pathlib import Path

from . import l4
from .loaders import LOADERS
from .model import ReportError, combine
from .privacy import check_output
from .render import report
from .semantic import attach


def parser():
    result = argparse.ArgumentParser(
        description="从任意 L1–L4 子集生成两份独立结果报告，不调用模型"
    )
    for layer in ("l1", "l2", "l3", "l4"):
        result.add_argument(
            f"--{layer}",
            type=Path,
            action="append",
            default=[],
            help="测量JSON；L4为含out/与eval/的根目录，可重复指定",
        )
    result.add_argument(
        "--llm",
        action="append",
        default=[],
        metavar="LAYER=DIRECTORY",
        help="可选 plan.json/stats.json 目录",
    )
    result.add_argument("--out", type=Path, required=True, help="只生成报告的输出目录，不覆盖输入")
    result.add_argument(
        "--forbid",
        action="append",
        default=[],
        help="禁止出现在报告中的额外个人/业务标识，不写入日志",
    )
    result.add_argument(
        "--verbose", action="store_true", help="私有 stderr 显示详细异常；可能包含输入路径，勿公开"
    )
    return result


def generate(args):
    rows = []
    inputs = []
    for layer in ("l1", "l2", "l3", "l4"):
        for path in getattr(args, layer):
            if not path.exists():
                raise ReportError(f"{layer.upper()} 输入不存在")
            inputs.append(path.resolve())
            if layer == "l3" and path.with_name("lifecycle.json").is_file():
                inputs.append(path.with_name("lifecycle.json").resolve())
            try:
                rows.extend(l4.load(path) if layer == "l4" else LOADERS[layer](path))
            except ReportError as error:
                raise ReportError(f"{layer.upper()} 输入：{error}") from error
    if not inputs:
        raise ReportError("至少明确提供一层测试数据")
    if not rows:
        raise ReportError("所选输入没有可展示的测量行")
    for path in inputs:
        if path == args.out.resolve() or path.is_dir() and args.out.resolve().is_relative_to(path):
            raise ReportError("输出目录不能位于输入运行目录内")
    llm = []
    for value in args.llm:
        layer, separator, directory = value.partition("=")
        if not separator or layer not in LOADERS:
            raise ReportError("--llm 使用 l1/l2/l3=目录")
        llm.append((layer, Path(directory)))
    outputs = [
        (args.out / name).resolve()
        for name in ("tokenless-report_zh.md", "comparison-report_zh.md")
    ]
    protected = inputs + [
        path.resolve()
        for _, directory in llm
        for path in (directory / "plan.json", directory / "stats.json")
    ]
    for destination in outputs:
        if any(
            destination == source
            or (source.is_dir() and destination.is_relative_to(source))
            or destination.exists()
            and source.exists()
            and destination.samefile(source)
            for source in protected
        ):
            raise ReportError("固定报告文件不能覆盖原始输入或模型测题证据")
    rows = attach(combine(rows), llm)
    documents = {
        "tokenless-report_zh.md": report(rows),
        "comparison-report_zh.md": report(rows, comparison=True),
    }
    for text in documents.values():
        check_output(text, args.forbid)
    args.out.mkdir(parents=True, exist_ok=True)
    pending = []
    try:
        for name, text in documents.items():
            descriptor, temporary = tempfile.mkstemp(prefix=".report-", dir=args.out)
            with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
                stream.write(text.rstrip() + "\n")
            pending.append((Path(temporary), args.out / name))
        for temporary, destination in pending:
            os.replace(temporary, destination)
    finally:
        for temporary, _ in pending:
            temporary.unlink(missing_ok=True)
    return documents


def main(argv=None):
    args = parser().parse_args(argv)
    try:
        generate(args)
    except (ReportError, OSError, TypeError, KeyError, AttributeError, ValueError) as error:
        message = (
            str(error)
            if isinstance(error, ReportError)
            else f"输入处理失败：{type(error).__name__}"
        )
        print(message, file=sys.stderr)
        if args.verbose:
            traceback.print_exc(file=sys.stderr)
        return 2
    print("已生成独立结果报告；未知值和未运行层保留为未测。")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
