"""Optional QwenPaw plugin isolation acceleration shared by launch and SDK entries."""

import os


def _native_norm(path):
    # Compile the verified seam locally so its fingerprint follows the Python ABI.
    return os.path.normcase(os.path.realpath(str(path)))


def accelerate_paths(version):
    """Keep native plugin cleanup while resolving existing paths in libc."""
    # shortcut: verified 2.1.1b3 isolation seam; review before extending versions.
    if version != "2.1.1b3":
        return
    try:
        from qwenpaw.plugins import module_isolation
    except ImportError:
        return

    original = getattr(module_isolation, "_norm", None)
    if not callable(original):
        return
    if getattr(original, "_aw_native_paths", False):
        return original
    code = getattr(original, "__code__", None)
    if any(
        getattr(code, field, None) != getattr(_native_norm.__code__, field)
        for field in (
            "co_code",
            "co_consts",
            "co_names",
            "co_argcount",
            "co_kwonlyargcount",
            "co_freevars",
        )
    ):
        return
    try:
        import ctypes

        libc = ctypes.CDLL(None)
        libc.realpath.argtypes = (ctypes.c_char_p, ctypes.c_void_p)
        libc.realpath.restype = ctypes.c_void_p
        libc.free.argtypes = (ctypes.c_void_p,)
        libc.free.restype = None
    except (ImportError, OSError, AttributeError):
        # Optional acceleration must not add a native runtime requirement.
        return

    def normalize(path):
        value = str(path)
        if "\0" in value:
            return original(value)
        resolved = libc.realpath(os.fsencode(value), None)
        if resolved is None:
            return original(value)
        try:
            return os.path.normcase(os.fsdecode(ctypes.string_at(resolved)))
        finally:
            libc.free(resolved)

    normalize._aw_native_paths = True
    module_isolation._norm = normalize
    return normalize


def prepare_runtime() -> bool:
    """Detect the native runtime without importing the App or starting services."""
    from importlib.metadata import PackageNotFoundError, version

    try:
        native_version = version("qwenpaw")
    except PackageNotFoundError:
        return False
    return accelerate_paths(native_version) is not None
