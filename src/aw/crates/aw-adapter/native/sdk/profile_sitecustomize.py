"""AW-owned startup shim; delegate to the remaining application sitecustomize."""

import importlib.machinery
import importlib.util
import os
import sys

_directory = os.path.dirname(__file__)
# Import the SDK from the application's environment, after removing this shim.
sys.path[:] = [entry for entry in sys.path if os.path.abspath(entry) != _directory]
_original = os.environ.get("PYTHONPATH")
if _original is not None:
    _remaining = [entry for entry in _original.split(os.pathsep) if entry != _directory]
    if _remaining:
        os.environ["PYTHONPATH"] = os.pathsep.join(_remaining)
    else:
        os.environ.pop("PYTHONPATH", None)

try:
    from aw_sdk.profile_preload import preload

    preload()
except BaseException as _error:  # noqa: BLE001
    # SystemExit from sitecustomize becomes a CPython fatal error with a traceback.
    # Startup must stop before the application sees unguarded entry points.
    _code = getattr(_error, "code", "source_profile_bootstrap_failed")
    os.write(2, f"AW: {_code}\n".encode())
    os._exit(1)

_spec = importlib.machinery.PathFinder.find_spec(__name__, sys.path)
if _spec is not None and _spec.loader is not None:
    _module = importlib.util.module_from_spec(_spec)
    sys.modules[__name__] = _module
    try:
        _spec.loader.exec_module(_module)
    except BaseException:
        sys.modules.pop(__name__, None)
        raise
