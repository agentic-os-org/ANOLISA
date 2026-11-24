# A shell must reject -c at the function declaration before reaching any imports.
def write_result():
    import json
    import sys

    result = {"entry": "custom"}
    if generated_console(sys.argv[2]):
        try:
            version = installed_version(sys.argv[1])
        except (OSError, ImportError, SyntaxError, ValueError):
            # A dynamic version still permits the recognized managed console launch.
            version = None
        result = {"entry": "managed", "version": version}

    with open(sys.argv[3], "w", encoding="utf-8") as stream:
        json.dump(result, stream)


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


def generated_console(source):
    import ast

    # Parse bytes so PEP 263 transforms match runpy's execution of the console.
    try:
        tree = ast.parse(source.encode("utf-8"))
    except (SyntaxError, ValueError):
        return False
    if (tree.body and isinstance(tree.body[0], ast.Expr)
            and isinstance(tree.body[0].value, ast.Constant)
            and isinstance(tree.body[0].value.value, str)):
        # UV's relocatable shell/Python preamble is a Python module docstring.
        tree.body.pop(0)
    return ast.dump(tree) in {
        ast.dump(ast.parse(prefix + ENTRY.format(normalize=normalize)))
        for prefix, normalize in NORMALIZERS
    }


def installed_version(console):
    import ast
    import importlib.util
    import os
    import sys

    # Match the original console's import path, cwd and interpreter environment.
    if not sys.flags.safe_path:
        sys.path[0] = os.path.dirname(os.path.realpath(console))
    spec = importlib.util.find_spec("qwenpaw")
    if spec is None or spec.origin is None or spec.submodule_search_locations is None:
        raise ValueError("QwenPaw package is unavailable")
    path = os.path.join(os.path.dirname(spec.origin), "__version__.py")
    with open(path, "rb") as stream:
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
    write_result()
except Exception:
    # EX_SOFTWARE identifies a Python probe failure even if the FIFO is unwritable.
    raise SystemExit(70)
