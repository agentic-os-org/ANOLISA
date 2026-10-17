#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for xlsx_reader.py UTF-16 CSV/TSV support.

Regression test: Excel's "Unicode Text" / CSV Unicode exports are UTF-16 with a
BOM. The reader's encoding trial chain (utf-8-sig, gbk, utf-8, latin-1) had no
UTF-16 entry — utf-8/gbk reject the BOM bytes, but latin-1 accepts ANY byte
pair, so a UTF-16 file was silently decoded into mojibake (NUL-laced column
names and garbage values) and reported as a successful read.
"""

import json
import os
import subprocess
import sys
import tempfile
import unittest

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(SCRIPTS_DIR, "xlsx_reader.py")

HEADER = "订单号,商品,数量"
ROW = "1001,机械键盘,3"


def run_reader(path: str) -> subprocess.CompletedProcess:
    return subprocess.run([sys.executable, SCRIPT, path, "--json"],
                          capture_output=True, text=True, encoding="utf-8")


class TestUtf16Csv(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()

    def tearDown(self):
        self._tmp.cleanup()

    def _write(self, name: str, text: str, encoding: str) -> str:
        path = os.path.join(self._tmp.name, name)
        with open(path, "w", encoding=encoding, newline="") as f:
            f.write(text)
        return path

    def test_utf16_le_bom_csv_decodes_correctly(self):
        """修复前：UTF-16 BOM 文件被 latin-1 静默误读成乱码（列名含 NUL），
        读取"成功"但数据全错。修复后按 BOM 识别 UTF-16 正确解码。
        """
        path = self._write("orders.csv", HEADER + "\r\n" + ROW + "\r\n", "utf-16")
        result = run_reader(path)
        self.assertEqual(result.returncode, 0, result.stderr)
        data = json.loads(result.stdout)
        preview = json.dumps(data, ensure_ascii=False)
        self.assertIn("订单号", preview)
        self.assertIn("机械键盘", preview)

    def test_utf16_bom_tsv_decodes_correctly(self):
        """TSV 的 Excel Unicode 导出同样是 UTF-16 BOM。"""
        path = self._write("orders.tsv",
                           HEADER.replace(",", "\t") + "\r\n" + ROW.replace(",", "\t") + "\r\n",
                           "utf-16")
        result = run_reader(path)
        self.assertEqual(result.returncode, 0, result.stderr)
        preview = json.dumps(json.loads(result.stdout), ensure_ascii=False)
        self.assertIn("机械键盘", preview)

    def test_utf8_sig_csv_still_decodes(self):
        """正常链路保护：无 BOM 的常规编码尝试链行为不变。"""
        path = self._write("plain.csv", HEADER + "\n" + ROW + "\n", "utf-8-sig")
        result = run_reader(path)
        self.assertEqual(result.returncode, 0, result.stderr)
        preview = json.dumps(json.loads(result.stdout), ensure_ascii=False)
        self.assertIn("机械键盘", preview)


if __name__ == "__main__":
    unittest.main()
