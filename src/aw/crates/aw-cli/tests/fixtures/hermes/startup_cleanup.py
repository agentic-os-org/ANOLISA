"""Release real service registrations while the guarded native owner stays alive."""

import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
from types import SimpleNamespace


aw, root, source = sys.argv[1:]
root = Path(root)


def command(*args: str) -> dict:
    result = subprocess.run([aw, *args], capture_output=True, text=True, timeout=12)
    assert result.returncode == 0, result.stderr
    return json.loads(result.stdout) if result.stdout else {}


spec = importlib.util.spec_from_file_location("aw_startup", source)
startup = importlib.util.module_from_spec(spec)
spec.loader.exec_module(startup)
cases = []
for mode in ("return", "exit-zero", "exit", "exception", "import", "validation"):
    prepared = command("instance", "prepare", "--config", str(root / "aw.json"), "--agent", "sdk")
    directory = Path(prepared["binding"]).parent
    status = command("status", "--config", str(root / "aw.json"))["result"]
    assert status["bindings"] == 1, status
    guard = directory / "hermes-entrypoint.py"
    if mode == "validation":
        launch = directory / "hermes-launch.json"
        launch.write_text(json.dumps({"schema": "aw-hermes/v1alpha1"}))
        launch.chmod(0o600)
        guard.write_text("def main(): pass\n")
        guard.chmod(0o600)
        descriptor = root / "bootstrap.json"
        descriptor.write_text(json.dumps({"binary": aw, "config": str(root / "aw.json"),
            "profile": str(root), "agent": "hermes"}))
        descriptor.chmod(0o600)
        reply = {**prepared, "command": {"args": []},
                 "environment": {"AW_HERMES_LAUNCH": str(launch)}}
        original = subprocess.Popen

        def preparation_output(argv: list[str], **kwargs: object) -> subprocess.Popen:
            if argv[1:3] == ["instance", "prepare"]:
                return original([sys.executable, "-c", "print(" + repr(json.dumps(reply)) + ")"], **kwargs)
            return original(argv, **kwargs)

        subprocess.Popen = preparation_output
        try:
            startup.prepare(str(descriptor))
            raise AssertionError("invalid preparation was accepted")
        except ValueError as error:
            assert "guarded command" in str(error), error
        finally:
            subprocess.Popen = original
    else:
        ending = {"return": "return", "exit-zero": "raise SystemExit(0)", "exit": "raise SystemExit(7)",
                  "exception": "raise RuntimeError('native failure')"}.get(mode, "")
        guard.write_text("raise RuntimeError('guard import failure')\n" if mode == "import" else (
            "import os\nfrom pathlib import Path\n"
            "def main():\n"
            "    assert Path(__file__).parent.joinpath('__pycache__').is_dir()\n"
            "    " + ending + "\n"))
        guard.chmod(0o600)
        native = SimpleNamespace(main=lambda: None)
        startup._wrap(native, ({"binary": aw}, prepared, guard))
        # Exercise ordinary Python startup, including generated guard bytecode.
        previous = sys.dont_write_bytecode
        sys.dont_write_bytecode = False
        try:
            native.main()
            assert mode == "return"
        except SystemExit as error:
            assert (mode == "exit" and error.code == 7) or (mode == "exit-zero" and error.code == 0)
        except RuntimeError as error:
            assert mode in ("exception", "import") and "failure" in str(error)
        finally:
            sys.dont_write_bytecode = previous
    # This owner is still running: the service reaper cannot hide a missing release.
    status = command("status", "--config", str(root / "aw.json"))["result"]
    assert status["bindings"] == status["events"] == 0, status
    assert not directory.exists(), directory
    cases.append(mode)
print(json.dumps({"cases": cases, "owner_alive": os.getpid(), "native_assets_removed": True}))
