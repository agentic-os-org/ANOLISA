# Copyright 2026 Alibaba Cloud
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""Mock services cap their own address space, without a `preexec_fn`.

`Popen` runs `preexec_fn` between fork and exec, where a process with live
threads can deadlock before the child reaches its command; batch startup
starts the orphan-sweep thread before launching mock services. The ceiling is
now applied by the child itself, so these tests check both halves: the child
really is limited, and the launcher passes no callback.
"""

from __future__ import annotations

import signal
import subprocess
import sys
import unittest
from pathlib import Path
from unittest import mock

SRC = Path(__file__).resolve().parents[1] / "src"
if str(SRC) not in sys.path:
    sys.path.insert(0, str(SRC))

from ce_runner import parallel  # noqa: E402  (path set up above)

SERVICE_ARGV = ["mock-openclaw", "--serve", "--flag=value"]


class LimitedCommandTest(unittest.TestCase):
    def test_wraps_the_service_argv_unchanged(self) -> None:
        wrapped = parallel.limited_command(list(SERVICE_ARGV))
        self.assertEqual(wrapped[0], parallel._PYTHON)
        self.assertEqual(wrapped[-len(SERVICE_ARGV):], SERVICE_ARGV)
        self.assertIn(str(parallel.MOCK_SERVICE_RLIMIT_AS), wrapped[2])

    def test_child_applies_the_address_space_limit(self) -> None:
        probe = (
            "import resource;"
            "print(resource.getrlimit(resource.RLIMIT_AS)[0])"
        )
        result = subprocess.run(
            parallel.limited_command([sys.executable, "-c", probe]),
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=60, check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr.decode("utf-8", "replace"))
        self.assertEqual(
            int(result.stdout.decode("utf-8").strip()), parallel.MOCK_SERVICE_RLIMIT_AS
        )


class LauncherTest(unittest.TestCase):
    def test_launcher_passes_no_preexec_callback(self) -> None:
        service = {
            "name": "mock-openclaw",
            "port": 9999,
            "command": " ".join(SERVICE_ARGV),
            "health_check": "http://localhost:9999/health",
            "health_check_method": "GET",
            "env": {},
        }
        healthy = mock.Mock(status_code=200)

        with mock.patch.object(parallel, "load_task_yaml", return_value={"services": [service]}), \
                mock.patch.object(parallel.httpx, "get",
                                  side_effect=[OSError("not running"), healthy]), \
                mock.patch.object(parallel, "subprocess") as fake_subprocess, \
                mock.patch.object(parallel.time, "sleep"):
            parallel.start_mock_services_with_offset("tasks/T001/task.yaml", "/tmp/proj", 0)

        fake_subprocess.Popen.assert_called_once()
        argv = fake_subprocess.Popen.call_args.args[0]
        kwargs = fake_subprocess.Popen.call_args.kwargs
        self.assertNotIn("preexec_fn", kwargs, "a fork-to-exec callback is still installed")
        self.assertEqual(argv[0], parallel._PYTHON)
        self.assertEqual(argv[-len(SERVICE_ARGV):], SERVICE_ARGV)



class InheritedSignalDispositionTest(unittest.TestCase):
    """The shim is a second interpreter, so it restores what Python ignored.

    `Popen` restores SIGPIPE and SIGXFSZ in the child before exec, but this
    shim is itself that child: the interpreter ignores both again at startup
    and `execvp` keeps an ignored disposition. A non-Python service launched
    through the wrapper would therefore start with both ignored, while the
    same service launched directly starts with them restored.
    """

    @staticmethod
    def _ignored_mask(argv: list[str]) -> int:
        probe = ["/bin/sh", "-c", "grep -E '^SigIgn' /proc/self/status"]
        result = subprocess.run(
            [*argv, *probe],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=60, check=False,
        )
        if result.returncode != 0:
            raise AssertionError(result.stderr.decode("utf-8", "replace"))
        return int(result.stdout.decode("utf-8").split(":")[1].strip(), 16)

    @unittest.skipUnless(
        Path("/proc/self/status").exists(), "needs /proc to read a child mask"
    )
    def test_wrapped_child_matches_a_direct_launch(self) -> None:
        direct = self._ignored_mask([])
        wrapped = self._ignored_mask(parallel.limited_command([]))
        for name in ("SIGPIPE", "SIGXFSZ"):
            signum = getattr(signal, name, None)
            if signum is None:
                continue
            bit = 1 << (signum - 1)
            self.assertEqual(
                bool(wrapped & bit), bool(direct & bit),
                f"{name} disposition differs from a direct launch "
                f"(wrapped mask {wrapped:#x}, direct {direct:#x})",
            )


if __name__ == "__main__":
    unittest.main()
