"""Build the optional QwenPaw SDK adapter wheel from canonical Adapter sources."""

from pathlib import Path
import shutil

from setuptools import setup
from setuptools.command.build_py import build_py

ROOT = Path(__file__).resolve().parent
NATIVE = ROOT / "../../../crates/aw-adapter/native/qwenpaw"
OUTPUT = ROOT / "../../../target/python-sdk-qwenpaw"
(OUTPUT / "metadata").mkdir(parents=True, exist_ok=True)


class Build(build_py):
    def run(self) -> None:
        package = Path(self.build_lib) / "aw_sdk_qwenpaw"
        if package.exists():
            shutil.rmtree(package)
        super().run()
        native = package / "_native"
        native.mkdir(parents=True, exist_ok=True)
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
        shutil.copyfile(NATIVE / "../claude-sdk/aw_scope.py", native / "aw_scope.py")
        shutil.copyfile(ROOT / "../../../../../LICENSE", package / "LICENSE")


setup(
    cmdclass={"build_py": Build},
    options={
        "build": {"build_base": str(OUTPUT / "build")},
        "egg_info": {"egg_base": str(OUTPUT / "metadata")},
    },
)
