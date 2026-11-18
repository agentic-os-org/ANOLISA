"""Build the wheel with canonical Adapter sources from a clean AW checkout."""

from pathlib import Path
import shutil

from setuptools import setup
from setuptools.command.build_py import build_py

ROOT = Path(__file__).resolve().parent
NATIVE = ROOT / "../../crates/aw-adapter/native/qwenpaw"
OUTPUT = ROOT / "../../target/python-sdk"
(OUTPUT / "metadata").mkdir(parents=True, exist_ok=True)


class Build(build_py):
    def run(self) -> None:
        super().run()
        package = Path(self.build_lib) / "aw_sdk"
        native = package / "_native/qwenpaw"
        native.mkdir(parents=True, exist_ok=True)
        (native.parent / "__init__.py").write_text("")
        (native / "__init__.py").write_text("")
        for name in (
            "sdk.py",
            "aw_paths.py",
            "configured.py",
            "deferred.py",
            "plugin.py",
            "aw_skill.py",
            "aw_submission.py",
            "after_only.py",
            "after_resident.py",
            "aw_resident.py",
            "aw_native.py",
            "aw_private.py",
            "aw_mcp.py",
        ):
            shutil.copyfile(NATIVE / name, native / name)
        claude_source = NATIVE / "../claude-sdk"
        shutil.copyfile(claude_source / "aw_scope.py", native / "aw_scope.py")
        claude = package / "_native/claude"
        claude.mkdir(parents=True, exist_ok=True)
        (claude / "__init__.py").write_text("")
        for name in (
            "aw_claude.py",
            "aw_invocations.py",
            "aw_mcp.py",
            "aw_model.py",
            "aw_model_stream.py",
            "aw_resident.py",
            "aw_scope.py",
            "aw_sdk_scope.py",
            "aw_submission.py",
            "aw_transport.py",
        ):
            shutil.copyfile(claude_source / name, claude / name)
        shutil.copyfile(ROOT / "../../../../LICENSE", package / "LICENSE")


setup(
    cmdclass={"build_py": Build},
    options={
        "build": {"build_base": str(OUTPUT / "build")},
        "egg_info": {"egg_base": str(OUTPUT / "metadata")},
    },
)
