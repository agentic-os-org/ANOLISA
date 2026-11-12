"""Recognize generated consoles and read versions without importing QwenPaw."""

import ast
import importlib.util
import json
from pathlib import Path
import sys

ENTRY = """import sys
from qwenpaw.cli.main import cli
if __name__ == '__main__':
{normalize}    sys.exit(cli())
"""
# Only generated argument normalization may precede the native CLI call.
NORMALIZERS = (
    ("", ""),
    ("", "    sys.argv[0] = sys.argv[0].removesuffix('.exe')\n"),
    ("", """    if sys.argv[0].endswith('-script.pyw'):
        sys.argv[0] = sys.argv[0][:-11]
    elif sys.argv[0].endswith('.exe'):
        sys.argv[0] = sys.argv[0][:-4]
"""),
    ("import re\n", "    sys.argv[0] = re.sub(r'(-script\\.pyw|\\.exe)?$', '', sys.argv[0])\n"),
)
# Parse bytes so PEP 263 transforms match runpy's execution of the console.
tree = ast.parse(sys.argv[2].encode("utf-8"))
if (tree.body and isinstance(tree.body[0], ast.Expr)
        and isinstance(tree.body[0].value, ast.Constant)
        and isinstance(tree.body[0].value.value, str)):
    # UV's relocatable shell/Python preamble is a Python module docstring.
    tree.body.pop(0)
if ast.dump(tree) not in {
    ast.dump(ast.parse(prefix + ENTRY.format(normalize=normalize)))
    for prefix, normalize in NORMALIZERS
}:
    raise ValueError("QwenPaw console has custom startup behavior")


def installed_version():
    # Match the original console's import path, cwd and interpreter environment.
    if not sys.flags.safe_path:
        sys.path[0] = str(Path(sys.argv[1]).resolve().parent)
    spec = importlib.util.find_spec("qwenpaw")
    if spec is None or spec.origin is None or spec.submodule_search_locations is None:
        raise ValueError("QwenPaw package is unavailable")
    path = Path(spec.origin).parent / "__version__.py"
    with path.open("rb") as stream:
        source = stream.read(4097)
    if len(source) > 4096:
        raise ValueError("QwenPaw version file exceeds its limit")
    tree = ast.parse(source)
    if (len(tree.body) != 1 or not isinstance(tree.body[0], ast.Assign)
            or len(tree.body[0].targets) != 1
            or not isinstance(tree.body[0].targets[0], ast.Name)
            or tree.body[0].targets[0].id != "__version__"
            or not isinstance(tree.body[0].value, ast.Constant)
            or not isinstance(tree.body[0].value.value, str)):
        raise ValueError("QwenPaw version is not a literal assignment")
    return "QwenPaw, version " + tree.body[0].value.value


try:
    version = installed_version()
except (OSError, ImportError, SyntaxError, ValueError):
    version = None
# A dynamic version still permits the recognized managed console launch.
print(json.dumps({"version": version}))
