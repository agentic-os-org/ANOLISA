#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for test-module.sh cleanup and failure reporting.

Regression tests for two defect classes, exercised with stubbed kernel
commands (no root, no real modules, plain CI container):
1. any failure between insmod and rmmod left the module loaded with no
   message, and the pre-existing-module path swallowed rmmod failures and
   then died three steps later with a cryptic insmod "File exists";
2. a failing rmmod at the unload step aborted under set -e without the
   diagnosis step that was supposed to run.

Plus the review-round guards: a failed load must not be reported as
STILL LOADED by the trap, a similarly named module (fakemod_extra) must
not be taken for ours by a prefix match, and the kernel's reason for a
failed rmmod must reach the terminal at the trap and step-2 call sites.
"""

import os
import shutil
import subprocess
import tempfile
import unittest

TESTS_DIR = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.normpath(os.path.join(
    TESTS_DIR, os.pardir, "devops", "kernel-dev", "scripts", "test-module.sh"))

LSMOD_LOADED = "fakemod 16384 0 - Live 0x0000000000000000 (O)\n"
LSMOD_OTHER = "nvme 40960 3 - Live 0x0000000000000000\n"


class Sandbox:
    """A copy of the script + fixture .ko + stubbed kernel commands.

    The stubs share a state file that insmod/rmmod toggle, so lsmod reports
    the module exactly as the (stubbed) kernel would.
    """

    def __init__(self, tmp):
        self.root = tmp
        self.bin = os.path.join(tmp, "bin")
        self.skill = os.path.join(tmp, "skill")
        self.log = os.path.join(tmp, "stub.log")
        self.state = os.path.join(tmp, "loaded.state")
        os.makedirs(self.bin)
        os.makedirs(os.path.join(self.skill, "scripts"))
        os.makedirs(os.path.join(self.skill, "examples", "fakemod"))
        shutil.copy(SCRIPT, os.path.join(self.skill, "scripts", "test-module.sh"))
        with open(os.path.join(self.skill, "examples", "fakemod", "fakemod.ko"), "wb"):
            pass
        self._write("loaded.state", "unloaded")

    def _write(self, name, content):
        with open(os.path.join(self.root, name), "w") as f:
            f.write(content + "\n")

    def _stub(self, name, body):
        path = os.path.join(self.bin, name)
        with open(path, "w") as f:
            f.write(body)
        os.chmod(path, 0o755)

    def install(self, insmod_code=0, rmmod_code=0, sleep_code=0,
                dmesg_code=0, dmesg_stdout="fakemod: hello from the module",
                dmesg_stderr="", rmmod_stderr="", initially_loaded=False):
        if initially_loaded:
            self._write("loaded.state", "loaded")
        self._stub("insmod", f"""#!/bin/bash
echo "insmod $*" >> "{self.log}"
if [ {insmod_code} -eq 0 ]; then echo loaded > "{self.state}"; fi
exit {insmod_code}
""")
        self._stub("rmmod", f"""#!/bin/bash
echo "rmmod $*" >> "{self.log}"
if [ {rmmod_code} -eq 0 ]; then echo unloaded > "{self.state}"; fi
if [ -n '{rmmod_stderr}' ]; then printf '%s\\n' '{rmmod_stderr}' >&2; fi
exit {rmmod_code}
""")
        self._stub("lsmod", f"""#!/bin/bash
if [ "$(cat {self.state})" = "loaded" ]; then
  printf '%s' '{LSMOD_LOADED}'
else
  printf '%s' '{LSMOD_OTHER}'
fi
""")
        self._stub("modinfo", f'#!/bin/bash\necho "modinfo $*" >> "{self.log}"\nexit 0\n')
        self._stub("sleep", f"""#!/bin/bash
echo "sleep $*" >> "{self.log}"
exit {sleep_code}
""")
        self._stub("dmesg", f"""#!/bin/bash
echo "dmesg" >> "{self.log}"
printf '%s\\n' '{dmesg_stdout}'
printf '%s\\n' '{dmesg_stderr}' >&2
exit {dmesg_code}
""")

    def run(self, *args):
        env = dict(
            os.environ,
            PATH=self.bin + os.pathsep + os.environ.get("PATH", ""),
            KERNEL_TEST_SKIP_ROOT="1",
        )
        return subprocess.run(
            ["bash", os.path.join(self.skill, "scripts", "test-module.sh"), *args],
            capture_output=True,
            text=True,
            env=env,
        )

    def calls(self, command):
        if not os.path.exists(self.log):
            return []
        with open(self.log) as f:
            return [line for line in f.read().splitlines()
                    if line.startswith(command)]


class TestModuleScript(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.sandbox = Sandbox(self._tmp.name)

    def tearDown(self):
        self._tmp.cleanup()

    def test_happy_path_unloads_module(self):
        self.sandbox.install()
        result = self.sandbox.run("fakemod")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.sandbox.calls("rmmod"), ["rmmod fakemod"],
                         "exactly one rmmod, from step 8")
        self.assertIn("All tests passed", result.stdout)

    def test_failure_after_load_triggers_cleanup(self):
        # sleep fails at step 7 (first sleep call on a clean system):
        # set -e aborts between insmod and the step-8 rmmod.
        self.sandbox.install(sleep_code=1)
        result = self.sandbox.run("fakemod")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.sandbox.calls("rmmod"), ["rmmod fakemod"],
                         "the trap must attempt the unload the script never reached")
        self.assertIn("attempting rmmod", result.stdout)
        self.assertIn("cleanup handler", result.stdout)

    def test_cleanup_reports_stuck_module(self):
        self.sandbox.install(rmmod_code=1, sleep_code=1)
        result = self.sandbox.run("fakemod")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("STILL LOADED", result.stdout)
        self.assertIn("lsmod | grep", result.stdout)

    def test_step8_rmmod_failure_is_diagnosed(self):
        # The step-8 rmmod fails; step 7's sleep succeeded, so the failure
        # is reported at the unload step itself, not swallowed by set -e.
        self.sandbox.install(rmmod_code=1)
        result = self.sandbox.run("fakemod")
        self.assertEqual(result.returncode, 1)
        self.assertIn("rmmod failed — module remains loaded", result.stdout)
        self.assertIn("_exit", result.stdout,
                      "the diagnosis must point at the module exit path")
        # The trap runs after the step-8 failure and retries once.
        self.assertEqual(len(self.sandbox.calls("rmmod")), 2)

    def test_pristine_system_not_touched_by_trap(self):
        # Step 8 succeeds; a failing final dmesg must NOT re-trigger the
        # trap's rmmod (MODULE_LOADED was cleared).
        self.sandbox.install(dmesg_code=1, dmesg_stdout="",
                             dmesg_stderr="dmesg: read kernel buffer failed")
        result = self.sandbox.run("fakemod")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.sandbox.calls("rmmod"), ["rmmod fakemod"],
                         "the trap must be a no-op after a successful unload")

    def test_preexisting_module_unload_failure_fails_fast(self):
        self.sandbox.install(rmmod_code=1, initially_loaded=True)
        result = self.sandbox.run("fakemod")
        self.assertEqual(result.returncode, 1)
        self.assertIn("Cannot unload the already-loaded module", result.stdout)
        self.assertEqual(self.sandbox.calls("rmmod"), ["rmmod fakemod"])
        self.assertEqual(self.sandbox.calls("insmod"), [],
                         "step 2 must fail before any insmod")

    def test_dmesg_restricted_stays_readable(self):
        self.sandbox.install(dmesg_code=1, dmesg_stdout="",
                             dmesg_stderr="dmesg: read kernel buffer failed: Operation not permitted")
        result = self.sandbox.run("fakemod")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertNotIn("read kernel buffer failed", result.stdout)
        self.assertNotIn("read kernel buffer failed", result.stderr)
        self.assertIn("No logs found", result.stdout)

    def test_failed_load_does_not_claim_still_loaded(self):
        # insmod exits 0 but lsmod never lists the module: step 4's else
        # branch must clear MODULE_LOADED, or the trap contradicts the
        # verdict it just printed with a STILL LOADED pointing at the very
        # command that found nothing.
        self.sandbox.install(rmmod_code=1)
        self.sandbox._stub(
            "lsmod", "#!/bin/bash\nprintf '%s' \"" + LSMOD_OTHER + "\"\n")
        result = self.sandbox.run("fakemod")
        self.assertEqual(result.returncode, 1)
        self.assertIn("Module failed to load", result.stdout)
        self.assertNotIn("STILL LOADED", result.stdout)
        self.assertEqual(self.sandbox.calls("rmmod"), [],
                         "the trap must not attempt an unload the script "
                         "just observed unnecessary")

    def test_similarly_named_module_is_not_taken_for_ours(self):
        # lsmod lists fakemod_extra, not fakemod: the first-field match must
        # not fire step 2's fail-fast, so the test body still reaches
        # insmod where a prefix grep aborted on an rmmod that cannot
        # succeed.
        self.sandbox.install(rmmod_code=1, initially_loaded=True)
        self.sandbox._stub(
            "lsmod",
            "#!/bin/bash\nprintf '%s' \"fakemod_extra 16384 0 - Live "
            "0x0000000000000000\\n\"\n")
        result = self.sandbox.run("fakemod")
        self.assertEqual(self.sandbox.calls("rmmod"), [],
                         "step 2 must not fire on a prefix-similar name")
        self.assertTrue(self.sandbox.calls("insmod"),
                        "the test body must still run")
        self.assertIn("Module is not loaded", result.stdout)

    def test_rmmod_stderr_reaches_the_terminal(self):
        # The kernel's reason for a failed rmmod is the diagnosis: neither
        # the trap nor the step-2 call site may discard it.
        reason = "rmmod: ERROR: Module fakemod is in use"
        with tempfile.TemporaryDirectory() as tmp:
            box = Sandbox(tmp)
            box.install(rmmod_code=1, sleep_code=1, rmmod_stderr=reason)
            result = box.run("fakemod")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(reason, result.stderr,
                          "the trap site must not swallow the reason")
            self.assertIn("STILL LOADED", result.stdout)
        with tempfile.TemporaryDirectory() as tmp:
            box = Sandbox(tmp)
            box.install(rmmod_code=1, initially_loaded=True,
                        rmmod_stderr=reason)
            result = box.run("fakemod")
            self.assertEqual(result.returncode, 1)
            self.assertIn(reason, result.stderr,
                          "the step-2 site must not swallow the reason")
            self.assertIn("Cannot unload the already-loaded module",
                          result.stdout)


if __name__ == "__main__":
    unittest.main()
