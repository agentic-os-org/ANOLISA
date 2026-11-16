"""Optional native App integration; import only in applications using QwenPaw."""

__all__ = ["AfterOnlyPlugin", "QwenPawPlugin"]


def __getattr__(name: str):
    if name == "AfterOnlyPlugin":
        from aw_sdk._native.qwenpaw.after_only import AfterOnlyPlugin

        return AfterOnlyPlugin
    if name == "QwenPawPlugin":
        from aw_sdk._native.qwenpaw.sdk import QwenPawPlugin

        return QwenPawPlugin
    raise AttributeError(name)
