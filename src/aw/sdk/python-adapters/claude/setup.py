"""Build the optional Claude SDK adapter wheel from canonical Adapter sources."""

from pathlib import Path
import shutil

from setuptools import setup
from setuptools.command.build_py import build_py

ROOT = Path(__file__).resolve().parent
NATIVE = ROOT / "../../../crates/aw-adapter/native/claude-sdk"
OUTPUT = ROOT / "../../../target/python-sdk-claude"
(OUTPUT / "metadata").mkdir(parents=True, exist_ok=True)


class Build(build_py):
    def run(self) -> None:
        package = Path(self.build_lib) / "aw_sdk_claude"
        if package.exists():
            shutil.rmtree(package)
        super().run()
        native = package / "_native"
        native.mkdir(parents=True, exist_ok=True)
        (native / "__init__.py").write_text("")
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
            shutil.copyfile(NATIVE / name, native / name)
        shutil.copyfile(ROOT / "../../../../../LICENSE", package / "LICENSE")


setup(
    cmdclass={"build_py": Build},
    options={
        "build": {"build_base": str(OUTPUT / "build")},
        "egg_info": {"egg_base": str(OUTPUT / "metadata")},
    },
)
