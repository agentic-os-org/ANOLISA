"""Whole-CLI normalization cases using an already installed real jq.

Every fix case uses jq >= 1.0.0 and exercises its already-available path.
Missing/obsolete binaries, package-manager replacements and installations are
outside this suite. The check cases only list a private adjacent spec.
"""

import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


class EnvFixNormalizationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls: type["EnvFixNormalizationTests"]) -> None:
        cls.process_env = os.environ.copy()
        for name in tuple(cls.process_env):
            if name in ("BASH_ENV", "ENV", "SHELLOPTS", "BASHOPTS") or name.startswith(
                "BASH_FUNC_"
            ):
                cls.process_env.pop(name, None)
        source = os.environ.get("ENV_FIX_TEST_SOURCE")
        cls.source = (
            Path(source).resolve()
            if source
            else Path(__file__).resolve().parents[1]
            / "adapters/tokenless/common/tokenless-env-fix.sh"
        )
        cls.bash = os.environ.get("ENV_FIX_TEST_BASH") or shutil.which("bash")
        jq = os.environ.get("ENV_FIX_TEST_JQ")
        if not cls.source.is_file() or not cls.bash or not jq:
            raise RuntimeError(
                "Provide the complete script, real Bash and ENV_FIX_TEST_JQ; "
                "missing runtime is not a product regression"
            )
        cls.jq = Path(jq).resolve()
        if not cls.jq.is_file():
            raise RuntimeError("ENV_FIX_TEST_JQ must name the existing real jq CLI")
        if cls.jq.name not in ("jq", "jq.exe"):
            raise RuntimeError("ENV_FIX_TEST_JQ must retain the filename jq or jq.exe")
        version = subprocess.run(
            [str(cls.jq), "--version"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=10,
            check=False,
            env=cls.process_env,
        )
        banner = version.stdout.decode("utf-8").strip()
        parts = banner.removeprefix("jq-").split(".")
        # The unchanged installer recognizes three-component versions only.
        # Require that existing format so these cases cannot request an install.
        if (
            version.returncode != 0
            or not banner.startswith("jq-")
            or len(parts) != 3
            or not all(part.isdigit() for part in parts)
            or int(parts[0]) < 1
        ):
            raise RuntimeError(
                "These non-installing cases require real jq >= 1.0.0 with a "
                "three-component version banner"
            )
        resolution = subprocess.run(
            [
                cls.bash,
                "--noprofile",
                "--norc",
                "-c",
                'export PATH="$(cd "$2" && pwd):$PATH"; '
                "found=$(command -v jq) || exit 1; "
                '[ -f "$found" ] && [ "$found" -ef "$1" ] || exit 1; '
                'printf "%s\\n" "$found"',
                "env-fix-runtime-preflight",
                cls.jq.as_posix(),
                cls.jq.parent.as_posix(),
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=10,
            check=False,
            env=cls.process_env,
        )
        if resolution.returncode != 0 or not resolution.stdout.strip():
            raise RuntimeError(
                "Real Bash must resolve jq to the supplied real CLI file"
            )

    def setUp(self: "EnvFixNormalizationTests") -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="env-fix-normalize-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.home = self.root / "home"
        self.home.mkdir()
        self.script = self.root / "tokenless-env-fix.sh"
        shutil.copyfile(self.source, self.script)

    def invoke(
        self: "EnvFixNormalizationTests", *arguments: str, stdin: str | None = None
    ) -> str:
        # Fixed environment setup only. The complete script remains the entry.
        # cd/pwd converts a real Windows jq directory for Git Bash's PATH.
        wrapper = (
            'export HOME="$(cd "$2" && pwd)"; '
            'export PATH="$(cd "$3" && pwd):$PATH"; '
        )
        if len(arguments) == 2 and arguments[0] == "fix-all" and arguments[1] != "-":
            argument_file = self.root / "argument.json"
            argument_file.write_bytes(arguments[1].encode("utf-8"))
            arguments = ("fix-all", argument_file.as_posix())
            wrapper += 'json=$(cat "$5") || exit 1; ' 'exec "$BASH" "$1" "$4" "$json"'
        else:
            wrapper += 'exec "$BASH" "$1" "${@:4}"'
        result = subprocess.run(
            [
                self.bash,
                "--noprofile",
                "--norc",
                "-c",
                wrapper,
                "env-fix-normalization",
                self.script.as_posix(),
                self.home.as_posix(),
                self.jq.parent.as_posix(),
                *arguments,
            ],
            input=(stdin or "").encode("utf-8"),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=30,
            check=False,
            env=self.process_env,
        )
        stdout = result.stdout.decode("utf-8")
        stderr = result.stderr.decode("utf-8")
        self.assertEqual(result.returncode, 0, (stdout, stderr))
        self.assertNotIn("attempting install", stdout)
        self.assertNotIn("installed via", stdout)
        self.assertFalse((self.home / ".tokenless/env-fix.log").exists())
        return stdout

    def write_spec(
        self: "EnvFixNormalizationTests", entries: list[str | dict[str, object]]
    ) -> None:
        spec = {
            "_meta": {"version": "2.0"},
            "Shell": {"required": entries, "recommended": []},
        }
        (self.root / "tool-ready-spec.json").write_text(
            json.dumps(spec), encoding="utf-8"
        )

    def test_fix_all_argument_splits_supported_constraint(
        self: "EnvFixNormalizationTests",
    ) -> None:
        output = self.invoke("fix-all", '["jq>=1.0.0"]')
        self.assertIn("jq: already available", output)
        self.assertIn("satisfies >=1.0.0", output)
        self.assertNotIn("BLOCKED", output)

    def test_fix_all_stdin_splits_supported_constraint(
        self: "EnvFixNormalizationTests",
    ) -> None:
        output = self.invoke("fix-all", "-", stdin='["jq>=1.0.0"]\n')
        self.assertIn("jq: already available", output)
        self.assertIn("satisfies >=1.0.0", output)
        self.assertNotIn("BLOCKED", output)

    def test_check_lists_binary_and_package_without_constraint(
        self: "EnvFixNormalizationTests",
    ) -> None:
        self.write_spec(["jq>=1.0.0"])
        output = self.invoke("check")
        lines = [line for line in output.splitlines() if "(package:" in line]
        self.assertEqual(len(lines), 1, output)
        self.assertTrue(lines[0].startswith("  jq — "), output)
        self.assertIn("(package: jq, fallbacks: 0)", lines[0])
        self.assertNotIn("jq>=1.0.0", lines[0])

    def test_fix_all_plain_string_still_uses_available_binary(
        self: "EnvFixNormalizationTests",
    ) -> None:
        output = self.invoke("fix-all", '["jq"]')
        self.assertIn("jq: already available", output)
        self.assertNotIn("satisfies", output)

    def test_fix_all_object_retains_existing_version_constraint(
        self: "EnvFixNormalizationTests",
    ) -> None:
        entry = {
            "binary": "jq",
            "package": "jq",
            "manager": "npm",
            "version": ">=1.0.0",
        }
        output = self.invoke("fix-all", json.dumps([entry]))
        self.assertIn("jq: already available", output)
        self.assertIn("satisfies >=1.0.0", output)

    def test_check_preserves_object_manager_package_and_fallback(
        self: "EnvFixNormalizationTests",
    ) -> None:
        self.write_spec(
            [
                {
                    "binary": "jq",
                    "package": "jq-package-control",
                    "manager": "npm",
                    "fallback": [{"method": "cargo", "package": "jq"}],
                }
            ]
        )
        output = self.invoke("check")
        self.assertIn("  jq — npm (package: jq-package-control, fallbacks: 1)", output)


if __name__ == "__main__":
    unittest.main()
