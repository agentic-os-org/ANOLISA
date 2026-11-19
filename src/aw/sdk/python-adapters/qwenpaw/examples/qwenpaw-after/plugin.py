"""Enable in the native profile; start and attach AW after application readiness."""

import os

from aw_sdk.qwenpaw import AfterOnlyPlugin

plugin = AfterOnlyPlugin(
    endpoint=os.environ["AW_SOCKET"],
    target="qwenpaw",
    attach_timeout_ms=5000,
)
