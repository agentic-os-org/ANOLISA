"""Read the installed literal version without executing QwenPaw initialization."""

import ast
import importlib.util
from pathlib import Path
import sys

# Match the original console script's import path, cwd and interpreter environment.
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
print("QwenPaw, version " + tree.body[0].value.value)
