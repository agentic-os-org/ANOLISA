"""Build the framework-free core AW Python SDK wheel from a clean AW checkout."""

from pathlib import Path
import shutil

from setuptools import setup
from setuptools.command.build_py import build_py

ROOT = Path(__file__).resolve().parent
OUTPUT = ROOT / "../../target/python-sdk"
(OUTPUT / "metadata").mkdir(parents=True, exist_ok=True)


class Build(build_py):
    def run(self) -> None:
        package = Path(self.build_lib) / "aw_sdk"
        if package.exists():
            shutil.rmtree(package)
        super().run()
        shutil.copyfile(ROOT / "../../../../LICENSE", package / "LICENSE")


setup(
    cmdclass={"build_py": Build},
    options={
        "build": {"build_base": str(OUTPUT / "build")},
        "egg_info": {"egg_base": str(OUTPUT / "metadata")},
    },
)
