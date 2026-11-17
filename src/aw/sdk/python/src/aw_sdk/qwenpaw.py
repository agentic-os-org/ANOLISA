"""Optional native App integration; import only in applications using QwenPaw."""

__all__ = ["AfterOnlyPlugin", "QwenPawPlugin", "prepare_runtime"]


def prepare_runtime() -> bool:
    """Install optional native acceleration before plugin loading; start no services.

    Returns whether the verified QwenPaw 2.1.1b3 path optimization is installed.
    Repeated calls are safe. Unsupported or absent runtimes keep native behavior.
    The patch lasts until process exit; changing it requires an App restart.
    """
    from aw_sdk._native.qwenpaw.aw_paths import prepare_runtime as prepare

    return prepare()


def __getattr__(name: str):
    if name in ("AfterOnlyPlugin", "QwenPawPlugin"):
        prepare_runtime()
    if name == "AfterOnlyPlugin":
        from aw_sdk._native.qwenpaw.after_only import AfterOnlyPlugin

        return AfterOnlyPlugin
    if name == "QwenPawPlugin":
        from aw_sdk._native.qwenpaw.sdk import QwenPawPlugin

        return QwenPawPlugin
    raise AttributeError(name)
